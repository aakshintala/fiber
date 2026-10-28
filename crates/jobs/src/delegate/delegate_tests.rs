//! Tests first for `delegate_spawn`: its definition, effects, argument
//! and model checks, and its receipt.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use contract::inbox::Delivery;
use contract::jobs::{JobRecord, Jobs as _};
use contract::tool::Tool as _;
use fakes::Deadline;
use fakes::{Recorder, Watchdog, within};

use super::DelegateSpawn;
use super::models::Resolve;
use super::run::{Fresh, Launch, Launched, Start, mint_session_id};
use super::test_rig::{DEADLINE, Rig, child_group, rig};
use crate::registry::Registry;

/// Receives the stopped delegate's notice and asserts it ended cancelled.
#[track_caller]
fn assert_cancelled_notice(rig: &Rig) {
    let Delivery::Job(notice) = Deadline::after(DEADLINE)
        .recv(&rig.inbox)
        .expect("the stopped delegate reports")
    else {
        panic!("job notice");
    };
    assert_eq!(
        notice.completed.status,
        contract::events::Outcome::Cancelled
    );
}

fn resolve() -> Resolve {
    Resolve {
        lookup: Arc::new(|model: &str| match model {
            "fiber:fake/m" => Ok("fake/m".to_owned()),
            "deep" => Ok("fake/m:high".to_owned()),
            _ => Err(format!("Unknown delegate model {model:?}.")),
        }),
        catalogue: Arc::from("Roles:\ndeep: fiber:fake/m:high\nReferences:\nfiber:fake/m"),
        credential: Arc::new(|_| Ok(None)),
    }
}

fn tool(rig: &Rig) -> DelegateSpawn {
    DelegateSpawn {
        wiring: rig.wiring(),
        resolve: resolve(),
    }
}

fn arguments(model: &str) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        "description": "scan",
        "prompt": "hi",
        "model": model,
    })
    .as_object()
    .unwrap()
    .clone()
}

/// The stub catalogue, shared with the refusal tests below: every
/// model refusal lists it after its reason.
const CATALOGUE: &str = "Roles:\ndeep: fiber:fake/m:high\nReferences:\nfiber:fake/m";

#[test]
fn isolation_and_workspace_are_validated_before_model_or_credentials() {
    let rig = rig();
    let mut spawned = tool(&rig);
    let lookups = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    spawned.resolve.lookup = Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Err("must not resolve".into())
    });
    spawned.resolve.credential = Arc::new(|_| panic!("must not resolve credentials"));
    let schema = spawned.definition().input_schema;
    assert_eq!(
        schema["properties"]["isolation"]["enum"],
        serde_json::json!(["none", "worktree"])
    );
    assert_eq!(schema["properties"]["workspace"]["type"], "string");
    for (key, value) in [
        ("isolation", serde_json::json!(1)),
        ("isolation", serde_json::json!("sometimes")),
        ("workspace", serde_json::json!(1)),
    ] {
        let mut args = arguments("unresolvable");
        args.insert(key.into(), value);
        let output = spawned.run(&args, &fakes::CancelToken::new(), &Recorder::default());
        assert_eq!(
            output.error.unwrap().code,
            contract::ErrorCode::InvalidArguments
        );
    }
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn explicit_none_isolation_starts_without_a_worktree() {
    let rig = rig();
    let spawned = tool(&rig);
    let mut args = arguments("fiber:fake/m");
    args.insert("isolation".into(), serde_json::json!("none"));
    let output = spawned.run(&args, &fakes::CancelToken::new(), &Recorder::default());
    // Guard first: the child is already running, so arm its watchdog
    // before any assertion that could fail and strand it.
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let [_, JobRecord::DelegateStarted(delegate)] = &output.jobs[..] else {
        panic!("a good call records its start: {:?}", output.jobs);
    };
    assert!(delegate.worktree.is_none());
    let launched = Deadline::after(DEADLINE)
        .recv(&rig.launched)
        .expect("the launcher ran");
    assert!(launched.worktree.is_none());
    assert_eq!(
        rig.launch_count.load(Ordering::SeqCst),
        1,
        "the non-isolated path launches without creating"
    );
    assert_eq!(rig.registry.stop_delegates(), 1);
    assert_cancelled_notice(&rig);
}

#[test]
fn extension_isolation_and_workspace_use_the_shared_creation_path() {
    let rig = rig();
    let mut spawned = tool(&rig);
    let called = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&called);
    spawned.wiring.create = Arc::new(
        #[allow(
            clippy::result_large_err,
            reason = "the jobs boundary returns its typed error by value"
        )]
        move |workspace, id| {
            assert_eq!(workspace, std::path::Path::new("/explicit/repository"));
            assert!(id.0.starts_with("s_"));
            counted.fetch_add(1, Ordering::SeqCst);
            Err(crate::JobsError::DelegateWorktree {
                failure: contract::shapes::Failure {
                    code: contract::ErrorCode::IoFailed,
                    message: "creation reached".into(),
                    retry_after_ms: None,
                    provider: None,
                },
            })
        },
    );
    let mut args = arguments("fiber:fake/m");
    args.insert("isolation".into(), serde_json::json!("worktree"));
    args.insert(
        "workspace".into(),
        serde_json::json!("/explicit/repository"),
    );
    let failure = spawned
        .start_extension("extension", &args, &mut |_| {})
        .unwrap_err();
    assert!(matches!(failure, crate::JobsError::DelegateWorktree { .. }));
    assert_eq!(called.load(Ordering::SeqCst), 1);
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_spawn_releases_the_parent_share_before_finishing_the_creation() {
    let rig = rig();
    let mut spawned = tool(&rig);
    let released = Arc::new(AtomicUsize::new(0));
    let ended = Arc::new(AtomicUsize::new(0));
    let release = Arc::clone(&released);
    spawned.wiring.create = Arc::new(
        #[allow(
            clippy::result_large_err,
            reason = "the jobs boundary returns its typed error by value"
        )]
        move |workspace, id| {
            let release = Arc::clone(&release);
            Ok((
                super::EndWorktree {
                    path: workspace.join(&id.0),
                    branch: format!("fiber/{}", id.0),
                    base: Some("base".into()),
                    remove: true,
                },
                super::WorktreeGuard::new(move || {
                    release.fetch_add(1, Ordering::SeqCst);
                }),
            ))
        },
    );
    let release = Arc::clone(&released);
    let end = Arc::clone(&ended);
    spawned.wiring.finish = Arc::new(move |record| {
        assert_eq!(release.load(Ordering::SeqCst), 1);
        assert!(record.remove);
        assert_eq!(record.base.as_deref(), Some("base"));
        end.fetch_add(1, Ordering::SeqCst);
        contract::events::FinishedWorktree {
            worktree: contract::shapes::Worktree {
                path: record.path.to_string_lossy().into_owned(),
                branch: record.branch,
                repository: None,
            },
            dirty: false,
        }
    });
    spawned.wiring.launch = Arc::new(|_| Command::new("/no/such/fiber"));
    let mut args = arguments("fiber:fake/m");
    args.insert("isolation".into(), serde_json::json!("worktree"));
    let output = spawned.run(&args, &fakes::CancelToken::new(), &Recorder::default());
    assert_eq!(output.error.unwrap().code, contract::ErrorCode::ToolError);
    assert_eq!(released.load(Ordering::SeqCst), 1);
    assert_eq!(ended.load(Ordering::SeqCst), 1);
    assert!(output.jobs.is_empty());
}

#[test]
fn a_non_fiber_model_and_an_unknown_one_fail_without_spawning() {
    for model in ["claude:opus", "fiber:fake/nope"] {
        let rig = rig();
        let spawned = tool(&rig);
        let output = spawned.run(
            &arguments(model),
            &fakes::CancelToken::new(),
            &Recorder::default(),
        );
        let error = output.error.expect("an unknown model fails");
        assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
        assert_eq!(
            error.message,
            format!("Unknown delegate model {model:?}.\n{CATALOGUE}")
        );
        // The model reads the failure in the content, not the error.
        let text = output.content.iter().map(text_of).collect::<String>();
        assert_eq!(text, error.message);
        assert!(output.jobs.is_empty());
        assert_eq!(
            rig.launch_count.load(Ordering::SeqCst),
            0,
            "no process starts"
        );
    }
}

#[test]
fn a_missing_model_fails_listing_the_roles_and_references() {
    let rig = rig();
    let spawned = tool(&rig);
    let mut call = arguments("fiber:fake/m");
    call.remove("model");
    let output = spawned.run(&call, &fakes::CancelToken::new(), &Recorder::default());
    let error = output.error.expect("a missing model fails");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(
        error.message,
        format!("Give model: a role or a full reference.\n{CATALOGUE}")
    );
    assert!(output.jobs.is_empty());
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn a_non_string_model_fails_listing_the_roles_and_references() {
    for model in [
        serde_json::json!(5),
        serde_json::json!(null),
        serde_json::json!(["deep"]),
        serde_json::json!({"model": "deep"}),
    ] {
        let rig = rig();
        let spawned = tool(&rig);
        let mut call = arguments("fiber:fake/m");
        call.insert("model".to_owned(), model);
        let output = spawned.run(&call, &fakes::CancelToken::new(), &Recorder::default());
        let error = output.error.expect("a non-string model fails");
        assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
        assert_eq!(
            error.message,
            format!("model must be a string.\n{CATALOGUE}")
        );
        assert!(output.jobs.is_empty());
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn effects_accept_a_call_without_a_model_or_with_a_non_string_one() {
    use contract::tool::Tool as _;
    let rig = rig();
    let spawned = tool(&rig);
    let mut plain = arguments("fiber:fake/m");
    plain.remove("model");
    let mut wrong = arguments("fiber:fake/m");
    wrong.insert("model".to_owned(), serde_json::json!(5));
    for call in [plain, wrong] {
        let effects = spawned.effects(&call).expect("model reaches run");
        assert_eq!(
            effects.declared.effects,
            [contract::shapes::Effect::Executes]
        );
        assert!(effects.always_reviewed);
    }
}

#[test]
fn the_schema_leaves_model_optional_and_untyped() {
    use contract::tool::Tool as _;
    let rig = rig();
    let definition = tool(&rig).definition();
    assert_eq!(
        definition.input_schema["required"],
        serde_json::json!(["description", "prompt"])
    );
    let model = &definition.input_schema["properties"]["model"];
    assert!(model.is_object());
    assert!(model.get("description").and_then(|d| d.as_str()).is_some());
    assert!(model.get("type").is_none(), "{model}");
    assert_eq!(
        definition.input_schema["properties"]["description"]["type"],
        serde_json::json!("string")
    );
    assert_eq!(
        definition.input_schema["properties"]["prompt"]["type"],
        serde_json::json!("string")
    );
    let encoded = serde_json::to_value(&definition).unwrap().to_string();
    assert!(!encoded.contains("deep"), "{encoded}");
    assert!(!encoded.contains("fiber:fake/m"), "{encoded}");
}

#[test]
fn a_role_starts_its_resolved_reference_and_records_it() {
    let rig = rig();
    let spawned = tool(&rig);
    let output = spawned.run(
        &arguments("deep"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    // Guard first: the child is already running, so arm its watchdog
    // before any assertion that could fail and strand it.
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let [
        JobRecord::Started(started),
        JobRecord::DelegateStarted(delegate),
    ] = &output.jobs[..]
    else {
        panic!("a good call records its start: {:?}", output.jobs);
    };
    assert_eq!(delegate.model, "fiber:fake/m:high");
    let launched = Deadline::after(DEADLINE)
        .recv(&rig.launched)
        .expect("the launcher ran");
    assert_eq!(launched.job_id, started.job_id);
    let Start::Fresh(Fresh::Spawn { model, .. }) = &launched.start else {
        panic!("a spawn launches fresh");
    };
    assert_eq!(model, "fake/m:high");
    assert_eq!(rig.registry.stop_delegates(), 1);
    assert_cancelled_notice(&rig);
}

#[test]
fn a_spawn_reads_its_inherited_roots_at_launch() {
    let rig = rig();
    let mut spawned = tool(&rig);
    // Read at launch, not at wiring: the roots below are added after
    // the tool was built, and the start still captures them. A spawn
    // folds the parent's current tail.
    let seen = Arc::new(Mutex::new(false));
    let seen_in = Arc::clone(&seen);
    let late = Arc::new(Mutex::new(Vec::new()));
    let late_in = Arc::clone(&late);
    spawned.wiring.inherited = Arc::new(move || {
        *seen_in.lock().unwrap() = true;
        late_in.lock().unwrap().clone()
    });
    late.lock().unwrap().push(PathBuf::from("/late"));
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    // Guard first: the child is already running, so arm its watchdog
    // before any assertion that could fail and strand it.
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    assert!(*seen.lock().unwrap(), "the roots are read at launch");
    let launched = Deadline::after(DEADLINE)
        .recv(&rig.launched)
        .expect("the launcher ran");
    assert_eq!(launched.added, [PathBuf::from("/late")]);
    assert_eq!(rig.registry.stop_delegates(), 1);
    assert_cancelled_notice(&rig);
}

#[test]
fn a_missing_argument_fails_without_spawning() {
    let rig = rig();
    let spawned = tool(&rig);
    let mut arguments = arguments("fiber:fake/m");
    arguments.remove("prompt");
    let output = spawned.run(&arguments, &fakes::CancelToken::new(), &Recorder::default());
    let error = output.error.expect("a missing argument fails");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn a_good_call_returns_its_receipt_with_started_records() {
    let rig = rig();
    let spawned = tool(&rig);
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    // Guard first: the child is already running, so arm its watchdog
    // before any assertion that could fail and strand it.
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    // The call records its start; the runner reports the end later, so
    // the answer carries exactly the two start records.
    let [
        JobRecord::Started(started),
        JobRecord::DelegateStarted(delegate),
    ] = &output.jobs[..]
    else {
        panic!("a good call records its start: {:?}", output.jobs);
    };
    assert_eq!(started.tool.as_deref(), Some("delegate_spawn"));
    assert_eq!(started.description, "scan");
    assert_eq!(delegate.harness, "fiber");
    assert_eq!(delegate.model, "fiber:fake/m");
    assert!(delegate.delegate_session_id.0.starts_with("s_"));
    let text = output.content.iter().map(text_of).collect::<String>();
    assert!(text.contains(&started.job_id.0), "{text}");
    assert!(text.contains(&started.output_path), "{text}");
    assert!(started.output_path.ends_with("events.jsonl"));
    assert!(std::path::Path::new(&started.output_path).is_absolute());
    // The launch the tool ran names the same job the record names, and
    // carries the parent the child's argv is built from.
    let launched = Deadline::after(DEADLINE)
        .recv(&rig.launched)
        .expect("the launcher ran");
    assert_eq!(launched.job_id, started.job_id);
    assert_eq!(launched.session_id, delegate.delegate_session_id);
    assert_eq!(launched.parent.0, "s_parent");
    let Start::Fresh(Fresh::Spawn { model, .. }) = &launched.start else {
        panic!("a spawn launches fresh");
    };
    assert_eq!(model, "fake/m");
    assert_eq!(rig.registry.stop_delegates(), 1);
    assert_cancelled_notice(&rig);
}

fn text_of(part: &contract::shapes::ContentPart) -> String {
    match part {
        contract::shapes::ContentPart::Text { text } => text.clone(),
        contract::shapes::ContentPart::Image { .. }
        | contract::shapes::ContentPart::Pdf(_)
        | contract::shapes::ContentPart::Unknown => String::new(),
    }
}

#[test]
fn a_spawn_error_fails_io_failed_with_no_job() {
    let rig = rig();
    let failing: Launch = Arc::new(|_: &Launched| Command::new("/nonexistent/fiber-test-binary"));
    let mut spawned = tool(&rig);
    spawned.wiring.launch = failing;
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let error = output.error.expect("a spawn error fails");
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    let text = output.content.iter().map(text_of).collect::<String>();
    assert!(
        text.contains("Starting the delegate failed"),
        "the model sees why: {text}"
    );
    assert!(output.jobs.is_empty());
    assert_eq!(rig.registry.list_text(), "No jobs.\n");
    assert!(
        rig.inbox.try_recv().is_err(),
        "nothing was recorded, so nothing is sent"
    );
}

#[test]
fn concurrent_spawn_cap_is_checked_after_model_validation() {
    for count in [9, 10] {
        let rig = rig();
        let mut finishes = Vec::new();
        for _ in 0..count {
            let id = crate::registry::mint_job_id();
            finishes.push(rig.registry.open_started(
                id.clone(),
                "delegate_spawn".into(),
                "fixture".into(),
                "/unused".into(),
                contract::jobs::Stop(Box::new(|| {})),
                crate::support::delegate_started(&id.0),
            ));
        }
        let spawned = tool(&rig);
        if count == 10 {
            rig.registry.install_timing(Arc::new(|_, _, _| {
                panic!("a refused start writes no jobs.start timing")
            }));
        } else {
            let mut failing = tool(&rig);
            failing.wiring.launch = Arc::new(|_| Command::new("/nonexistent/fiber-cap-test"));
            let output = failing.run(
                &arguments("fiber:fake/m"),
                &fakes::CancelToken::new(),
                &Recorder::default(),
            );
            assert_eq!(output.error.unwrap().code, contract::ErrorCode::ToolError);
            assert!(
                output
                    .content
                    .iter()
                    .map(text_of)
                    .collect::<String>()
                    .contains("Starting the delegate failed")
            );
        }
        let invalid = spawned.run(
            &arguments("unknown"),
            &fakes::CancelToken::new(),
            &Recorder::default(),
        );
        assert_eq!(
            invalid.error.unwrap().code,
            contract::ErrorCode::InvalidArguments
        );
        let output = spawned.run(
            &arguments("fiber:fake/m"),
            &fakes::CancelToken::new(),
            &Recorder::default(),
        );
        if count == 9 {
            let watchdog = child_group(&rig);
            assert!(output.error.is_none());
            assert_eq!(rig.launch_count.load(Ordering::SeqCst), 1);
            rig.registry.stop_delegates();
            watchdog.stand_down(fakes::Deadline::after(DEADLINE));
        } else {
            let error = output.error.unwrap();
            assert_eq!(error.code, contract::ErrorCode::ToolError);
            assert_eq!(
                error.message,
                "This session already runs 10 delegates, the most it can run at once, so nothing was started; wait for one with `jobs wait` or stop one with `jobs stop`, then try again."
            );
            assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn timeout_schema_and_argument_edges() {
    let rig = rig();
    let spawned = tool(&rig);
    let schema = spawned.definition().input_schema;
    let property = &schema["properties"]["timeout_ms"];
    assert_eq!(property["type"], "integer");
    assert_eq!(property["minimum"], 1);
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("timeout_ms"))
    );
    for value in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("5"),
        serde_json::Value::Null,
    ] {
        let mut call = arguments("fiber:fake/m");
        call.insert("timeout_ms".into(), value);
        assert!(
            matches!(spawned.effects(&call), Err(contract::tool::EffectsError::Arguments(message)) if message == "`timeout_ms` must be an integer of 1 or more.")
        );
        let output = spawned.run(&call, &fakes::CancelToken::new(), &Recorder::default());
        let error = output.error.unwrap();
        assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
        assert_eq!(
            error.message,
            "`timeout_ms` must be an integer of 1 or more."
        );
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
    }
    let mut call = arguments("fiber:fake/m");
    assert!(spawned.effects(&call).is_ok());
    for n in [1, 2] {
        call.insert("timeout_ms".into(), serde_json::json!(n));
        assert!(spawned.effects(&call).is_ok());
    }
}

#[test]
fn absent_timeout_keeps_the_delegate_running_after_a_poll() {
    use contract::clock::Clock as _;
    let rig = rig();
    let mut spawned = tool(&rig);
    let (release, held) = mpsc::channel::<()>();
    let held = std::sync::Mutex::new(held);
    spawned.wiring.watch = Arc::new(move |_, _, _| {
        let _released = held.lock().unwrap().recv();
        Err(std::io::Error::other("released"))
    });
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let deadline = Deadline::after(DEADLINE);
    let step = Duration::from_secs(1);
    rig.clock
        .advance_when_parked(Some(rig.clock.now() + step), None, step, &deadline);
    assert!(rig.inbox.try_recv().is_err());
    assert_eq!(rig.registry.stop_delegates(), 1);
    drop(release);
    assert_cancelled_notice(&rig);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn timeout_spawn_stops_and_reaps_its_child() {
    use contract::clock::Clock as _;
    let rig = rig();
    let spawned = tool(&rig);
    let mut call = arguments("fiber:fake/m");
    call.insert("timeout_ms".into(), serde_json::json!(1));
    let output = spawned.run(&call, &fakes::CancelToken::new(), &Recorder::default());
    let watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let deadline = Deadline::after(DEADLINE);
    let step = Duration::from_millis(1);
    rig.clock
        .advance_when_parked(Some(rig.clock.now() + step), None, step, &deadline);
    let Delivery::Job(notice) = deadline
        .recv(&rig.inbox)
        .expect("deadline ended the delegate")
    else {
        panic!("job notice");
    };
    assert_eq!(notice.completed.status, contract::events::Outcome::Failed);
    assert_eq!(
        notice.completed.error.unwrap().code,
        contract::ErrorCode::Timeout
    );
    assert!(notice.completed.process.unwrap().timed_out);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn eof_before_exit_reports_without_a_clock_move() {
    let rig = rig();
    let mut spawned = tool(&rig);
    let fifo = rig._dir.path().join("fifo");
    fakes::fifo(&fifo);
    let pid = rig._dir.path().join("child.pid");
    let fifo_arg = fifo.clone();
    spawned.wiring.launch = Arc::new(move |_: &Launched| {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!(
                "echo $$ > '{}'; exec >&-; read _ < '{}'; exit 0",
                pid.display(),
                fifo_arg.display()
            ),
        ]);
        command
    });
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let watchdog = child_group(&rig);
    assert!(output.error.is_none());
    // The child closed stdout before blocking on the fifo, so its exit
    // follows this write with no stdout event and no clock move: the
    // runner must still reap it and report.
    within(
        "the child reads the fifo",
        Deadline::after(DEADLINE),
        move || {
            std::fs::write(&fifo, "\n").unwrap();
        },
    );
    let deadline = Deadline::after(DEADLINE);
    let Delivery::Job(notice) = deadline
        .recv(&rig.inbox)
        .expect("the exited delegate reports")
    else {
        panic!("job notice");
    };
    assert_eq!(notice.completed.status, contract::events::Outcome::Failed);
    assert_eq!(
        notice.completed.error.unwrap().code,
        contract::ErrorCode::Indeterminate
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn depth_edges_refuse_by_tool_name() {
    assert!(super::depth_refusal(1, "delegate_spawn").is_none());
    for depth in [2, 3] {
        let error = super::depth_refusal(depth, "delegate_fork").unwrap();
        assert!(
            matches!(&error, crate::JobsError::DepthExceeded { tool } if tool == "delegate_fork"),
            "depth {depth}"
        );
        assert_eq!(error.code(), contract::ErrorCode::DepthExceeded);
    }
}

#[test]
fn depth_precedes_arguments_and_model_lookup_without_changing_the_definition() {
    for depth in [2, 3] {
        for model in ["fiber:fake/m", "unknown"] {
            let rig = rig();
            let mut spawned = tool(&rig);
            let definition = serde_json::to_value(spawned.definition()).unwrap();
            spawned.wiring.depth = depth;
            assert_eq!(
                serde_json::to_value(spawned.definition()).unwrap(),
                definition
            );
            let lookups = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&lookups);
            spawned.resolve.lookup = Arc::new(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Err("must not resolve".into())
            });
            for call in [arguments(model), serde_json::Map::new()] {
                let output = spawned.run(&call, &fakes::CancelToken::new(), &Recorder::default());
                assert_eq!(
                    output.error.unwrap().code,
                    contract::ErrorCode::DepthExceeded
                );
                assert_eq!(lookups.load(Ordering::SeqCst), 0);
                assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
                assert_eq!(rig.registry.list_text(), "No jobs.\n");
            }
        }
    }
    let rig = rig();
    let mut spawned = tool(&rig);
    spawned.wiring.depth = 1;
    let output = spawned.run(
        &arguments("unknown"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        output.error.unwrap().code,
        contract::ErrorCode::InvalidArguments
    );
}

#[test]
fn extension_starts_validate_depth_and_keys_before_binding_or_launching() {
    for (depth, extra, code) in [
        (2, true, contract::ErrorCode::DepthExceeded),
        (3, false, contract::ErrorCode::DepthExceeded),
        (1, true, contract::ErrorCode::InvalidArguments),
    ] {
        let rig = rig();
        let mut spawned = tool(&rig);
        spawned.wiring.depth = depth;
        let mut args = arguments("fiber:fake/m");
        if extra {
            args.insert("not_allowed".into(), serde_json::json!(true));
        }
        let mut bound = Vec::new();
        let error = spawned
            .start_extension("a", &args, &mut |id| bound.push(id.clone()))
            .unwrap_err();
        if depth >= 2 {
            assert!(
                matches!(&error, crate::JobsError::DepthExceeded { tool } if tool == "delegate_spawn"),
                "depth {depth}"
            );
        } else {
            assert!(
                matches!(&error, crate::JobsError::ArgumentsNotAllowed { keys } if keys == &["not_allowed".to_owned()]),
                "depth {depth}"
            );
        }
        assert_eq!(error.code(), code);
        assert!(bound.is_empty());
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
        assert!(contract::jobs::Jobs::running(rig.registry.as_ref()).is_empty());
    }
}

#[test]
fn start_extension_refuses_with_no_inbox() {
    let rig = rig();
    let _guard = Watchdog::matching(&rig.sessions.display().to_string());
    let mut spawned = tool(&rig);
    spawned.wiring.registry = Registry::new(
        rig.sessions.clone(),
        rig.clock.clone(),
        Arc::new(Recorder::default()),
    );
    let mut bound = Vec::new();
    let error = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.push(id.clone())
        })
        .unwrap_err();
    assert!(matches!(&error, crate::JobsError::NoInbox));
    assert_eq!(error.code(), contract::ErrorCode::ToolError);
    assert!(bound.is_empty());
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
    assert!(contract::jobs::Jobs::running(spawned.wiring.registry.as_ref()).is_empty());
    let counted = rig.launch_count.clone();
    spawned.wiring.launch = Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Command::new("/no/such/fiber")
    });
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        rig.launch_count.load(Ordering::SeqCst),
        1,
        "the model start still reaches spawn with no inbox"
    );
    assert!(
        output
            .error
            .unwrap()
            .message
            .starts_with("Starting the delegate failed: ")
    );
}

#[test]
fn extension_spawn_failure_binds_once_but_records_nothing() {
    let rig = rig();
    let mut spawned = tool(&rig);
    spawned.wiring.launch = Arc::new(|_| Command::new("/no/such/fiber"));
    let mut bound = Vec::new();
    let error = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.push(id.clone())
        })
        .unwrap_err();
    assert!(matches!(&error, crate::JobsError::SpawnFailed { .. }));
    assert_eq!(error.code(), contract::ErrorCode::ToolError);
    assert!(
        error
            .to_string()
            .starts_with("Starting the delegate failed: "),
        "the spawn's own reason follows the case's sentence"
    );
    assert_eq!(bound.len(), 1);
    assert!(contract::jobs::Jobs::running(rig.registry.as_ref()).is_empty());
    assert_eq!(rig.registry.list_text(), "No jobs.\n");
    assert!(rig.inbox.try_recv().is_err());
}

type Releases = Arc<std::sync::Mutex<Vec<std::fs::File>>>;

fn held_children(rig: &Rig) -> (Launch, Releases) {
    let releases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let held = releases.clone();
    let home = rig._dir.path().to_path_buf();
    let launch: Launch = Arc::new(move |child| {
        let path = home.join(format!("release-{}", child.job_id.0));
        fakes::fifo(&path);
        // Keep both ends open until the child consumes its release, even
        // when it has not opened the FIFO yet.
        let release = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        held.lock().unwrap().push(release);
        let mut command = Command::new("sh");
        command
            .args(["-c", "read line < \"$1\"", "held-child"])
            .arg(path);
        command
    });
    (launch, releases)
}

#[test]
fn an_immediate_exit_still_delivers_the_extension_start_before_its_end() {
    let rig = rig();
    let mut spawned = tool(&rig);
    spawned.wiring.launch = Arc::new(|_| {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        command
    });
    let id = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |_| {})
        .unwrap();
    let deadline = Deadline::after(DEADLINE);
    let Delivery::ExtensionJob(started, _) =
        deadline.recv_or_fail(&rig.inbox, "immediate child's start")
    else {
        panic!("start must precede end");
    };
    assert_eq!(started.job_id, id);
    let Delivery::Job(notice) = deadline.recv_or_fail(&rig.inbox, "immediate child's end") else {
        panic!("end follows start");
    };
    assert_eq!(notice.completed.job_id, id);
}

#[test]
fn extension_start_binds_before_launch_and_sends_start_before_end_at_depth_one() {
    let rig = rig();
    let mut spawned = tool(&rig);
    spawned.wiring.depth = 1;
    let bound = Arc::new(std::sync::Mutex::new(Vec::new()));
    let checked = bound.clone();
    let (launch, releases) = held_children(&rig);
    spawned.wiring.launch = Arc::new(move |child| {
        assert_eq!(child.depth, 2);
        assert_eq!(*checked.lock().unwrap(), vec![child.job_id.clone()]);
        launch(child)
    });
    let _guard = Watchdog::matching(&rig._dir.path().display().to_string());
    let id = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.lock().unwrap().push(id.clone())
        })
        .unwrap();
    let Delivery::ExtensionJob(started, delegate) = rig.inbox.try_recv().unwrap() else {
        panic!("start precedes the end");
    };
    assert_eq!(started.job_id, id);
    assert_eq!(delegate.job_id, id);
    assert_eq!(started.extension.as_deref(), Some("a"));
    assert_eq!(started.tool, None);
    assert!(rig.inbox.try_recv().is_err());
    use std::io::Write as _;
    for release in releases.lock().unwrap().iter_mut() {
        writeln!(release, "go").unwrap();
    }
    let Delivery::Job(notice) =
        Deadline::after(DEADLINE).recv_or_fail(&rig.inbox, "delegate end after release")
    else {
        panic!("end follows the start");
    };
    assert_eq!(notice.completed.job_id, id);
}

#[test]
fn both_starts_share_the_nine_ten_eleven_boundary_and_release_slots() {
    for extension_tenth in [false, true] {
        let rig = rig();
        let mut spawned = tool(&rig);
        let (launch, releases) = held_children(&rig);
        let counted = rig.launch_count.clone();
        spawned.wiring.launch = Arc::new(move |child| {
            counted.fetch_add(1, Ordering::SeqCst);
            launch(child)
        });
        let _guard = Watchdog::matching(&rig._dir.path().display().to_string());
        let shell = rig
            .registry
            .open(crate::support::opening("shell does not count"))
            .unwrap();
        let mut held = Vec::new();
        for n in 0..9 {
            let id = format!("j_{n}");
            let delegate = crate::support::delegate_started(&id);
            let finish = if n % 2 == 0 {
                rig.registry.open_extension_started(
                    contract::JobId(id),
                    "a".into(),
                    "scan".into(),
                    "/child/events.jsonl".into(),
                    contract::jobs::Stop(Box::new(|| {})),
                    delegate,
                )
            } else {
                rig.registry
                    .open_started(
                        contract::JobId(id),
                        "delegate_spawn".into(),
                        "scan".into(),
                        "/child/events.jsonl".into(),
                        contract::jobs::Stop(Box::new(|| {})),
                        delegate,
                    )
                    .1
            };
            held.push(finish);
        }
        assert_eq!(
            contract::jobs::Jobs::running(rig.registry.as_ref()).len(),
            10
        );
        let start = |extension| {
            if extension {
                spawned
                    .start_extension("a", &arguments("fiber:fake/m"), &mut |_| {})
                    .map(|_| ())
                    .map_err(|error| (error.code(), error.to_string()))
            } else {
                let output = spawned.run(
                    &arguments("fiber:fake/m"),
                    &fakes::CancelToken::new(),
                    &Recorder::default(),
                );
                output
                    .error
                    .map_or(Ok(()), |error| Err((error.code, error.message)))
            }
        };
        start(extension_tenth).unwrap();
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            contract::jobs::Jobs::running(rig.registry.as_ref()).len(),
            11
        );
        let error = start(!extension_tenth).unwrap_err();
        assert_eq!(error.0, contract::ErrorCode::ToolError);
        assert_eq!(error.1, crate::JobsError::DelegatesFull.to_string());
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 1);
        held.pop()
            .unwrap()
            .report(crate::support::ended_ok("ignored"), None);
        start(!extension_tenth).unwrap();
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 2);
        use std::io::Write as _;
        for release in releases.lock().unwrap().iter_mut() {
            writeln!(release, "go").unwrap();
        }
        // Drain the two real children as well as the synthetic starts and end.
        let deadline = Deadline::after(DEADLINE);
        let mut children = 0;
        while children < 2 {
            let delivery = deadline.recv_or_fail(&rig.inbox, "released cap fixture children ended");
            if let Delivery::Job(notice) = delivery
                && notice.completed.job_id.0 != "j_8"
            {
                children += 1;
            }
        }
        drop(shell);
    }
}

#[test]
fn minted_session_ids_differ_from_job_ids() {
    let session = mint_session_id();
    assert!(session.0.starts_with("s_"));
    assert_ne!(session.0, crate::registry::mint_job_id().0);
}

#[test]
fn a_stop_before_the_runner_connects_still_ends_cancelled() {
    let rig = rig();
    let spawned = tool(&rig);
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    // Guard first: the stop below races the runner, and any failing
    // assert must still clean up the child.
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    // Stopping at once races the runner's first watch; the stop wins and
    // the job ends cancelled once the runner reaps it.
    assert_eq!(rig.registry.stop_delegates(), 1);
    assert_cancelled_notice(&rig);
}

#[test]
fn a_role_credential_reaches_the_launcher() {
    let rig = rig();
    let mut spawned = tool(&rig);
    spawned.resolve.credential = Arc::new(|_| Ok(Some("work".to_owned())));
    let output = spawned.run(
        &arguments("deep"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    // Guard first: the child is already running, so arm its watchdog
    // before any assertion that could fail and strand it.
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let launched = Deadline::after(DEADLINE)
        .recv(&rig.launched)
        .expect("the launcher ran");
    let Start::Fresh(Fresh::Spawn { model, credential }) = &launched.start else {
        panic!("a spawn launches fresh");
    };
    assert_eq!(model, "fake/m:high");
    assert_eq!(*credential, Some("work".to_owned()));
    assert_eq!(rig.registry.stop_delegates(), 1);
    assert_cancelled_notice(&rig);
}

#[test]
fn a_full_reference_launches_with_no_credential() {
    let rig = rig();
    let spawned = tool(&rig);
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    // Guard first: the child is already running, so arm its watchdog
    // before any assertion that could fail and strand it.
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let launched = Deadline::after(DEADLINE)
        .recv(&rig.launched)
        .expect("the launcher ran");
    let Start::Fresh(Fresh::Spawn { model, credential }) = &launched.start else {
        panic!("a spawn launches fresh");
    };
    assert_eq!(model, "fake/m");
    assert_eq!(*credential, None);
    assert_eq!(rig.registry.stop_delegates(), 1);
    assert_cancelled_notice(&rig);
}

#[test]
fn a_missing_label_fails_credential_missing_and_spawns_nothing() {
    let rig = rig();
    let mut spawned = tool(&rig);
    spawned.resolve.credential = Arc::new(|_| {
        Err(
            "`fake` has no credential label `gone`. The labels for `fake` are: default, work"
                .to_owned(),
        )
    });
    let output = spawned.run(
        &arguments("deep"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let error = output.error.expect("a missing label fails");
    assert_eq!(error.code, contract::ErrorCode::CredentialMissing);
    // The refusal names the labels, and lists no catalogue: a label is
    // never added to it.
    assert_eq!(
        error.message,
        "`fake` has no credential label `gone`. The labels for `fake` are: default, work"
    );
    let text = output.content.iter().map(text_of).collect::<String>();
    assert_eq!(text, error.message);
    assert!(output.jobs.is_empty());
    assert_eq!(
        rig.launch_count.load(Ordering::SeqCst),
        0,
        "no process starts"
    );
    assert!(
        rig.launched.try_recv().is_err(),
        "the launcher never receives"
    );
    assert_eq!(rig.registry.list_text(), "No jobs.\n");
    assert!(
        rig.inbox.try_recv().is_err(),
        "nothing was recorded, so nothing is sent"
    );
}

#[test]
fn a_refused_model_never_asks_for_the_credential() {
    let rig = rig();
    let mut spawned = tool(&rig);
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    spawned.resolve.credential = Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    });
    let output = spawned.run(
        &arguments("unknown"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let error = output.error.expect("an unknown model fails");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "the credential check runs only after the lookup was Ok"
    );
    assert_eq!(
        rig.launch_count.load(Ordering::SeqCst),
        0,
        "no process starts"
    );
}

#[test]
fn a_start_held_past_the_sweep_is_refused_closed_with_no_bind_no_job_no_launch() {
    let rig = rig();
    let mut spawned = tool(&rig);
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let held_release = std::sync::Mutex::new(release_rx);
    spawned.resolve.credential = Arc::new(move |_| {
        let _sent = reached_tx.send(());
        let _released = held_release.lock().unwrap().recv();
        Ok(None)
    });
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bound = Vec::new();
        let result = spawned.start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.push(id.clone())
        });
        let _sent = result_tx.send((result.map(|id| id.0), bound));
    });
    let wait = Deadline::after(DEADLINE);
    wait.recv(&reached_rx)
        .expect("the start reached the pause point");
    // The registry half of the sweep: close, then the sweep's first read.
    rig.registry.close_extension_starts();
    assert!(
        contract::jobs::Jobs::running(rig.registry.as_ref()).is_empty(),
        "nothing runs yet"
    );
    drop(release_tx);
    let (result, bound) = wait.recv(&result_rx).expect("the start answered");
    let error = result.expect_err("the held start is refused");
    assert!(matches!(&error, crate::JobsError::ExtensionStartsClosed));
    assert_eq!(error.code(), contract::ErrorCode::Closing);
    assert!(bound.is_empty(), "bind never ran");
    assert!(
        contract::jobs::Jobs::running(rig.registry.as_ref()).is_empty(),
        "no job was recorded"
    );
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn a_close_waits_for_a_start_holding_the_gate_then_lists_it() {
    let rig = rig();
    let spawned = Arc::new(tool(&rig));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (result_tx, result_rx) = mpsc::channel();
    let starter = Arc::clone(&spawned);
    std::thread::spawn(move || {
        let mut bound = Vec::new();
        let result = starter.start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.push(id.clone());
            let _sent = entered_tx.send(());
            let _released = release_rx.recv();
        });
        let _sent = result_tx.send((result.map(|id| id.0), bound));
    });
    let wait = Deadline::after(DEADLINE);
    wait.recv(&entered_rx).expect("bind holds the gate");
    // The close takes the same gate, so it cannot return yet.
    let closed_registry = Arc::clone(&rig.registry);
    let (closed_tx, closed_rx) = mpsc::channel();
    std::thread::spawn(move || {
        closed_registry.close_extension_starts();
        let _sent = closed_tx.send(());
    });
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&closed_rx)
            .is_err(),
        "the close waits on the start gate"
    );
    drop(release_tx);
    let (result, bound) = wait.recv(&result_rx).expect("the start answered");
    let id = result.expect("the admitted start commits");
    assert_eq!(bound, vec![contract::JobId(id.clone())]);
    wait.recv(&closed_rx).expect("the close returned");
    let Delivery::ExtensionJob(started, _) =
        wait.recv(&rig.inbox).expect("the admitted start is sent")
    else {
        panic!("the admitted start is sent first");
    };
    assert_eq!(started.job_id.0, id);
    assert_eq!(
        contract::jobs::Jobs::running(rig.registry.as_ref()),
        vec![contract::JobId(id.clone())]
    );
    // A later extension start is refused, while a model start still runs.
    let _watchdog = child_group(&rig);
    let error = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |_| {})
        .expect_err("starts stay closed");
    assert!(matches!(&error, crate::JobsError::ExtensionStartsClosed));
    assert_eq!(error.code(), contract::ErrorCode::Closing);
    let output = spawned.run(
        &arguments("fiber:fake/m"),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "the model path is unchanged");
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 2);
    assert_eq!(rig.registry.stop_delegates(), 2);
    assert_cancelled_notice(&rig);
    assert_cancelled_notice(&rig);
}

#[test]
fn a_closed_start_at_the_cap_is_refused_closed_not_full() {
    let rig = rig();
    let mut held = Vec::new();
    for n in 0..10 {
        let id = contract::JobId(format!("j_{n}"));
        held.push(rig.registry.open_extension_started(
            id.clone(),
            "a".into(),
            "scan".into(),
            "/child/events.jsonl".into(),
            contract::jobs::Stop(Box::new(|| {})),
            crate::support::delegate_started(&id.0),
        ));
    }
    contract::jobs::Jobs::close_extension_starts(rig.registry.as_ref());
    let mut bound = Vec::new();
    let spawned = tool(&rig);
    let error = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.push(id.clone())
        })
        .expect_err("the closed start is refused");
    assert!(matches!(&error, crate::JobsError::ExtensionStartsClosed));
    assert_eq!(error.code(), contract::ErrorCode::Closing);
    assert!(bound.is_empty(), "bind never ran");
    assert_eq!(
        contract::jobs::Jobs::running(rig.registry.as_ref()).len(),
        10,
        "the listed ten stay"
    );
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}
