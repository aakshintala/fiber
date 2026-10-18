//! Tests first for `delegate_spawn`: its definition, effects, argument
//! and model checks, and its receipt.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use contract::inbox::Delivery;
use contract::jobs::{JobRecord, Jobs as _};
use contract::tool::Tool as _;
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{Recorder, TempDir, Watchdog, within};

use super::DelegateSpawn;
use super::models::Resolve;
use super::run::{Launch, Launched, Watch, mint_session_id};
use crate::registry::Registry;

/// How long a test waits on the wall clock before it fails.
const DEADLINE: Duration = Duration::from_secs(3);

struct Rig {
    _dir: TempDir,
    registry: Arc<Registry>,
    clock: Arc<FakeClock>,
    inbox: mpsc::Receiver<Delivery>,
    sessions: std::path::PathBuf,
    launched: mpsc::Receiver<Launched>,
    launch_count: Arc<AtomicUsize>,
    launch: Launch,
    watch: Watch,
}

fn rig() -> Rig {
    let dir = TempDir::new("fiber-delegate-spawn");
    let clock = FakeClock::new();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let sessions = dir.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let registry = Registry::new(
        artifacts,
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        Arc::new(Recorder::default()),
    );
    let (inbox_tx, inbox_rx) = mpsc::channel();
    registry.deliver_to(inbox_tx);
    let (launched_tx, launched_rx) = mpsc::channel();
    let launch_count = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&launch_count);
    let pid = dir.path().join("child.pid");
    let launch: Launch = Arc::new(move |launched: &Launched| {
        counted.fetch_add(1, Ordering::SeqCst);
        let _sent = launched_tx.send(Launched {
            depth: launched.depth,
            session_id: launched.session_id.clone(),
            job_id: launched.job_id.clone(),
            parent: launched.parent.clone(),
            model: launched.model.clone(),
            prompt: launched.prompt.clone(),
            workspace: launched.workspace.clone(),
            resume: false,
            credential: launched.credential.clone(),
        });
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!("echo $$ > '{}'; exec sleep 60", pid.display()),
        ]);
        command
    });
    let watch: Watch = Arc::new(|_, _, _| Err(std::io::Error::other("refused")));
    Rig {
        _dir: dir,
        registry,
        clock,
        inbox: inbox_rx,
        sessions,
        launched: launched_rx,
        launch_count,
        launch,
        watch,
    }
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

/// The `sleep` the rig's launcher started, read from its pidfile: armed
/// as its watchdog so a failing test leaves no child behind.
#[track_caller]
fn child_group(rig: &Rig) -> Watchdog {
    let pid = rig._dir.path().join("child.pid");
    let pid: u32 = within("the child writes its pid", DEADLINE, move || {
        loop {
            let Ok(text) = std::fs::read_to_string(&pid) else {
                std::thread::yield_now();
                continue;
            };
            let Ok(pid) = text.trim().parse() else {
                std::thread::yield_now();
                continue;
            };
            return pid;
        }
    });
    Watchdog::group(pid)
}

fn tool(rig: &Rig) -> DelegateSpawn {
    DelegateSpawn {
        depth: 0,
        registry: Arc::clone(&rig.registry),
        parent: contract::SessionId("s_parent".into()),
        workspace: rig.sessions.parent().unwrap().to_path_buf(),
        sessions: rig.sessions.clone(),
        clock: Arc::clone(&rig.clock) as Arc<dyn contract::clock::Clock>,
        bound: Duration::from_secs(5),
        cap: 1024,
        resolve: resolve(),
        launch: Arc::clone(&rig.launch),
        watch: Arc::clone(&rig.watch),
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
    assert_eq!(launched.model, "fake/m:high");
    // The stop reaches the running child; the clock lets the runner reap
    // it so no thread is left parked. The watchdog above already guards
    // it.
    assert_eq!(rig.registry.stop_delegates(), 1);
    rig.clock.advance(Duration::from_secs(6));
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
    assert_eq!(launched.model, "fake/m");
    // The stop reaches the running child; the clock lets the runner reap
    // it so no thread is left parked. The watchdog above already guards
    // it.
    assert_eq!(rig.registry.stop_delegates(), 1);
    rig.clock.advance(Duration::from_secs(6));
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
    let spawned = DelegateSpawn {
        depth: 0,
        registry: Arc::clone(&rig.registry),
        parent: contract::SessionId("s_parent".into()),
        workspace: rig.sessions.parent().unwrap().to_path_buf(),
        sessions: rig.sessions.clone(),
        clock: Arc::clone(&rig.clock) as Arc<dyn contract::clock::Clock>,
        bound: Duration::from_secs(5),
        cap: 1024,
        resolve: resolve(),
        launch: failing,
        watch: Arc::clone(&rig.watch),
    };
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
            failing.launch = Arc::new(|_| Command::new("/nonexistent/fiber-cap-test"));
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
            watchdog.stand_down(DEADLINE);
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
    spawned.watch = Arc::new(move |_, _, _| {
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
    let Delivery::Job(notice) = deadline.recv(&rig.inbox).expect("stopped delegate reports") else {
        panic!("job notice");
    };
    assert_eq!(
        notice.completed.status,
        contract::events::Outcome::Cancelled
    );
    watchdog.stand_down(DEADLINE);
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
    watchdog.stand_down(DEADLINE);
}

#[test]
fn eof_before_exit_reports_without_a_clock_move() {
    let rig = rig();
    let mut spawned = tool(&rig);
    let fifo = rig._dir.path().join("fifo");
    fakes::fifo(&fifo);
    let pid = rig._dir.path().join("child.pid");
    let fifo_arg = fifo.clone();
    spawned.launch = Arc::new(move |_: &Launched| {
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
    within("the child reads the fifo", DEADLINE, move || {
        std::fs::write(&fifo, "\n").unwrap();
    });
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
    watchdog.stand_down(DEADLINE);
}

#[test]
fn depth_edges_refuse_by_tool_name() {
    assert!(super::depth_refusal(1, "delegate_spawn").is_none());
    for depth in [2, 3] {
        let output = super::depth_refusal(depth, "delegate_fork").unwrap();
        let error = output.error.unwrap();
        assert_eq!(error.code, contract::ErrorCode::DepthExceeded);
        assert_eq!(
            error.message,
            "This delegate is at depth 2, the deepest a delegate can be, so it cannot use `delegate_fork`; do the work in this session."
        );
    }
}

#[test]
fn depth_precedes_arguments_and_model_lookup_without_changing_the_definition() {
    for depth in [2, 3] {
        for model in ["fiber:fake/m", "unknown"] {
            let rig = rig();
            let mut spawned = tool(&rig);
            let definition = serde_json::to_value(spawned.definition()).unwrap();
            spawned.depth = depth;
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
    spawned.depth = 1;
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
    for (depth, extra, code, message) in [
        (
            2,
            true,
            contract::ErrorCode::DepthExceeded,
            "This delegate is at depth 2, the deepest a delegate can be, so it cannot use `delegate_spawn`; do the work in this session.",
        ),
        (
            3,
            false,
            contract::ErrorCode::DepthExceeded,
            "This delegate is at depth 2, the deepest a delegate can be, so it cannot use `delegate_spawn`; do the work in this session.",
        ),
        (
            1,
            true,
            contract::ErrorCode::InvalidArguments,
            "`/isolation`: not allowed",
        ),
    ] {
        let rig = rig();
        let mut spawned = tool(&rig);
        spawned.depth = depth;
        let mut args = arguments("fiber:fake/m");
        if extra {
            args.insert("isolation".into(), serde_json::json!(true));
        }
        let mut bound = Vec::new();
        let error = spawned
            .start_extension("a", &args, &mut |id| bound.push(id.clone()))
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.message, message);
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
    spawned.registry = Registry::new(
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
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    assert_eq!(
        error.message,
        "No session is listening for this delegate's end, so nothing was started."
    );
    assert!(bound.is_empty());
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
    assert!(contract::jobs::Jobs::running(spawned.registry.as_ref()).is_empty());
    let counted = rig.launch_count.clone();
    spawned.launch = Arc::new(move |_| {
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
    spawned.launch = Arc::new(|_| Command::new("/no/such/fiber"));
    let mut bound = Vec::new();
    let error = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.push(id.clone())
        })
        .unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    assert!(error.message.starts_with("Starting the delegate failed: "));
    assert_eq!(bound.len(), 1);
    assert!(contract::jobs::Jobs::running(rig.registry.as_ref()).is_empty());
    assert_eq!(rig.registry.list_text(), "No jobs.\n");
    assert!(rig.inbox.try_recv().is_err());
}

#[test]
fn extension_start_binds_before_launch_and_sends_start_before_end_at_depth_one() {
    let rig = rig();
    let mut spawned = tool(&rig);
    spawned.depth = 1;
    let bound = Arc::new(std::sync::Mutex::new(Vec::new()));
    let checked = bound.clone();
    spawned.launch = Arc::new(move |launch| {
        assert_eq!(launch.depth, 2);
        assert_eq!(*checked.lock().unwrap(), vec![launch.job_id.clone()]);
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        command
    });
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    spawned.watch = Arc::new(move |_, _, _| {
        let _sent = entered_tx.send(());
        let _released = release_rx.lock().unwrap().recv();
        Err(std::io::Error::other("done"))
    });
    let _guard = Watchdog::matching(&rig.sessions.display().to_string());
    let id = spawned
        .start_extension("a", &arguments("fiber:fake/m"), &mut |id| {
            bound.lock().unwrap().push(id.clone())
        })
        .unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&entered_rx, "runner paused in watch");
    let Delivery::ExtensionJob(started, delegate) = rig.inbox.try_recv().unwrap() else {
        panic!("start precedes the end");
    };
    assert_eq!(started.job_id, id);
    assert_eq!(delegate.job_id, id);
    assert_eq!(started.extension.as_deref(), Some("a"));
    assert_eq!(started.tool, None);
    assert!(rig.inbox.try_recv().is_err());
    drop(release_tx);
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
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        spawned.watch = Arc::new(move |_, _, _| {
            let _released = release_rx.lock().unwrap().recv();
            Err(std::io::Error::other("done"))
        });
        let counted = rig.launch_count.clone();
        spawned.launch = Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        });
        let _guard = Watchdog::matching(&rig.sessions.display().to_string());
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
            } else {
                let output = spawned.run(
                    &arguments("fiber:fake/m"),
                    &fakes::CancelToken::new(),
                    &Recorder::default(),
                );
                output.error.map_or(Ok(()), Err)
            }
        };
        start(extension_tenth).unwrap();
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            contract::jobs::Jobs::running(rig.registry.as_ref()).len(),
            11
        );
        let error = start(!extension_tenth).unwrap_err();
        assert_eq!(error.code, contract::ErrorCode::ToolError);
        assert_eq!(error.message, crate::registry::runs::cap_message());
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 1);
        held.pop()
            .unwrap()
            .report(crate::support::ended_ok("ignored"), None);
        start(!extension_tenth).unwrap();
        assert_eq!(rig.launch_count.load(Ordering::SeqCst), 2);
        drop(release_tx);
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
    for _ in 0..200 {
        if let Ok(Delivery::Job(notice)) = rig.inbox.try_recv() {
            assert_eq!(
                notice.completed.status,
                contract::events::Outcome::Cancelled
            );
            rig.clock.advance(Duration::from_secs(6));
            return;
        }
        rig.clock.advance(Duration::from_millis(50));
        // Wall time for the runner thread: advances alone cost it none.
        // The wait itself may hold the notice, so it is read here too.
        if let Ok(Delivery::Job(notice)) =
            Deadline::after(Duration::from_millis(5)).recv(&rig.inbox)
        {
            assert_eq!(
                notice.completed.status,
                contract::events::Outcome::Cancelled
            );
            rig.clock.advance(Duration::from_secs(6));
            return;
        }
    }
    panic!("the stopped runner did not report");
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
    assert_eq!(launched.model, "fake/m:high");
    assert_eq!(launched.credential, Some("work".to_owned()));
    assert_eq!(rig.registry.stop_delegates(), 1);
    rig.clock.advance(Duration::from_secs(6));
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
    assert_eq!(launched.model, "fake/m");
    assert_eq!(launched.credential, None);
    assert_eq!(rig.registry.stop_delegates(), 1);
    rig.clock.advance(Duration::from_secs(6));
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
