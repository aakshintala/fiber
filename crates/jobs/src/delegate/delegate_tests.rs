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
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::Tool as _;
use fakes::clock::FakeClock;
use fakes::{Recorder, TempDir, Watchdog, within};

use super::DelegateSpawn;
use super::run::{Launch, Launched, Resolve, Watch, mint_session_id};
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
            session_id: launched.session_id.clone(),
            job_id: launched.job_id.clone(),
            parent: launched.parent.clone(),
            model: launched.model.clone(),
            prompt: launched.prompt.clone(),
            workspace: launched.workspace.clone(),
        });
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!("echo $$ > '{}'; exec sleep 60", pid.display()),
        ]);
        command
    });
    let watch: Watch = Arc::new(|_, _| Err(std::io::Error::other("refused")));
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
    Arc::new(|model: &str| {
        if model == "fiber:fake/m" {
            Ok("fake/m".to_owned())
        } else {
            Err(vec!["fiber:fake/m".to_owned()])
        }
    })
}

/// The `sleep` the rig's launcher started, read from its pidfile: armed
/// as its watchdog so a failing test leaves no child behind.
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

#[test]
fn the_definition_names_only_description_prompt_and_model() {
    let rig = rig();
    let definition = tool(&rig).definition();
    assert_eq!(definition.name, "delegate_spawn");
    let schema = &definition.input_schema;
    let properties = schema["properties"].as_object().unwrap();
    let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["description", "model", "prompt"]);
    assert_eq!(schema["additionalProperties"], false);
    let mut required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    required.sort_unstable();
    assert_eq!(required, ["description", "model", "prompt"]);
    // Neither the roles nor the references appear in the definition.
    let text = serde_json::to_string(&definition).unwrap();
    assert!(!text.contains("fiber:fake/m"), "{text}");
}

#[test]
fn the_effects_execute_and_skip_every_fast_path() {
    let rig = rig();
    let effects = tool(&rig).effects(&arguments("fiber:fake/m")).unwrap();
    assert_eq!(
        effects.declared,
        DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
        }
    );
    assert_eq!(effects.subject, Some(String::new()));
    assert_eq!(effects.prefix, None);
    assert!(effects.always_reviewed);
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
        assert!(
            error.message.contains("fiber:fake/m"),
            "it lists the valid references: {}",
            error.message
        );
        // The model reads the failure in the content, not the error.
        let text = output.content.iter().map(text_of).collect::<String>();
        assert!(text.contains(model), "{text}");
        assert!(text.contains("fiber:fake/m"), "{text}");
        assert!(output.jobs.is_empty());
        assert_eq!(
            rig.launch_count.load(Ordering::SeqCst),
            0,
            "no process starts"
        );
    }
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
    let launched = rig
        .launched
        .recv_timeout(DEADLINE)
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
    assert_eq!(error.code, contract::ErrorCode::IoFailed);
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
        if let Ok(Delivery::Job(notice)) = rig.inbox.recv_timeout(Duration::from_millis(5)) {
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
