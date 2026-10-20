//! Tests first for `delegate_fork`: its order, depth, cap boundaries,
//! point action and schema.

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
use contract::shapes::{Failure, Point};
use contract::tool::{Answered, Ask, Asking, Tool as _};
use contract::{ActionId, Seq};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{Recorder, TempDir, Watchdog, within};

use super::super::run::{Launch, Launched, Watch};
use super::super::{DelegateFork, ForkPoint, Forking};
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
    point_calls: Arc<AtomicUsize>,
    seen_actions: Arc<std::sync::Mutex<Vec<Option<ActionId>>>>,
    launch: Launch,
    watch: Watch,
}

fn forking() -> Forking {
    Forking {
        point: Point {
            session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".into()),
            seq: Seq(41),
        },
        model: "fiber:fake/m:high".to_owned(),
    }
}

fn rig() -> Rig {
    let dir = TempDir::new("fiber-delegate-fork");
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
    let point_calls = Arc::new(AtomicUsize::new(0));
    let seen_actions = Arc::new(std::sync::Mutex::new(Vec::new()));
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
        point_calls,
        seen_actions,
        launch,
        watch,
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

fn point_ok(rig: &Rig) -> ForkPoint {
    let calls = Arc::clone(&rig.point_calls);
    let seen = Arc::clone(&rig.seen_actions);
    let from = forking();
    Arc::new(move |action: Option<&ActionId>| {
        calls.fetch_add(1, Ordering::SeqCst);
        seen.lock().unwrap().push(action.cloned());
        Ok(from.clone())
    })
}

fn tool_with(rig: &Rig, point: ForkPoint) -> DelegateFork {
    DelegateFork {
        depth: 0,
        registry: Arc::clone(&rig.registry),
        parent: contract::SessionId("s_parent".into()),
        workspace: rig.sessions.parent().unwrap().to_path_buf(),
        sessions: rig.sessions.clone(),
        clock: Arc::clone(&rig.clock) as Arc<dyn contract::clock::Clock>,
        bound: Duration::from_secs(5),
        cap: 1024,
        point,
        launch: Arc::clone(&rig.launch),
        watch: Arc::clone(&rig.watch),
    }
}

fn tool(rig: &Rig) -> DelegateFork {
    let point = point_ok(rig);
    tool_with(rig, point)
}

fn arguments() -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        "description": "scan",
        "prompt": "hi",
    })
    .as_object()
    .unwrap()
    .clone()
}

/// An `Ask` carrying this call's action.
struct FakeAsk {
    action: ActionId,
}

impl Ask for FakeAsk {
    fn action(&self) -> ActionId {
        self.action.clone()
    }

    fn answerable(&self) -> bool {
        false
    }

    fn ask(&self, _: Asking) -> Answered {
        Answered::NoAnswer
    }
}

#[test]
fn a_depth_two_caller_fails_depth_exceeded_without_calling_the_point() {
    let rig = rig();
    let mut forked = tool(&rig);
    forked.depth = 2;
    let output = forked.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let error = output.error.expect("a depth-2 fork fails");
    assert_eq!(error.code, contract::ErrorCode::DepthExceeded);
    // The same refusal `delegate_spawn` uses, naming this tool: a fork
    // that skips the check or inlines its own cap number reads
    // differently.
    assert_eq!(
        error.message,
        super::super::depth_refusal(2, "delegate_fork")
            .expect("depth 2 is refused")
            .to_string()
    );
    assert!(output.jobs.is_empty());
    assert_eq!(rig.point_calls.load(Ordering::SeqCst), 0);
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn a_fork_at_the_cap_is_refused_like_a_spawn() {
    let rig = rig();
    let mut finishes = Vec::new();
    for _ in 0..10 {
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
    let forked = tool(&rig);
    let output = forked.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let error = output.error.expect("a fork at 10 live delegates fails");
    // Refused at 10, with the same cap text a spawn gets: the point ran
    // first (it resolves before admission), but nothing started.
    assert_eq!(rig.point_calls.load(Ordering::SeqCst), 1);
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    assert_eq!(error.message, crate::JobsError::DelegatesFull.to_string());
    drop(finishes);
}

#[test]
fn a_started_fork_counts_toward_the_cap() {
    let rig = rig();
    let mut finishes = Vec::new();
    for _ in 0..9 {
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
    let forked = tool(&rig);
    let output = forked.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    // Guard first: the child is already running, so arm its watchdog
    // before any assertion that could fail and strand it.
    let watchdog = child_group(&rig);
    assert!(output.error.is_none());
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 1);
    // 9 spawns plus one started fork: the next spawn is refused.
    let spawned = super::super::DelegateSpawn {
        depth: 0,
        registry: Arc::clone(&rig.registry),
        parent: contract::SessionId("s_parent".into()),
        workspace: rig.sessions.parent().unwrap().to_path_buf(),
        sessions: rig.sessions.clone(),
        clock: Arc::clone(&rig.clock) as Arc<dyn contract::clock::Clock>,
        bound: Duration::from_secs(5),
        cap: 1024,
        resolve: super::super::models::Resolve {
            lookup: Arc::new(|model: &str| match model {
                "fiber:fake/m" => Ok("fake/m".to_owned()),
                _ => Err(format!("Unknown delegate model {model:?}.")),
            }),
            catalogue: Arc::from("References:\nfiber:fake/m"),
            credential: Arc::new(|_| Ok(None)),
        },
        launch: Arc::clone(&rig.launch),
        watch: Arc::clone(&rig.watch),
    };
    let refused = spawned.run(
        &serde_json::json!({
            "description": "scan",
            "prompt": "hi",
            "model": "fiber:fake/m",
        })
        .as_object()
        .unwrap()
        .clone(),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let error = refused.error.expect("the next spawn is refused");
    assert_eq!(error.code, contract::ErrorCode::ToolError);
    assert_eq!(error.message, crate::JobsError::DelegatesFull.to_string());
    rig.registry.stop_delegates();
    watchdog.stand_down(DEADLINE);
    drop(finishes);
}

#[test]
fn the_point_runs_after_the_argument_check_and_before_admission() {
    let rig = rig();
    let forked = tool(&rig);
    // Bad arguments never invoke the point and start nothing.
    let mut bad = arguments();
    bad.remove("prompt");
    let output = forked.run(&bad, &fakes::CancelToken::new(), &Recorder::default());
    let error = output.error.expect("bad arguments fail");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(rig.point_calls.load(Ordering::SeqCst), 0);
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
    // A point failure starts nothing, carrying the reader's own code.
    let failing: ForkPoint = Arc::new(|_: Option<&ActionId>| {
        Err(Failure {
            code: contract::ErrorCode::InvalidArguments,
            message: "no forkable point".to_owned(),
            retry_after_ms: None,
            provider: None,
        })
    });
    let forked = tool_with(&rig, failing);
    let output = forked.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let error = output.error.expect("a point failure fails");
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert!(
        error.message.contains("no forkable point"),
        "{message}",
        message = error.message
    );
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn the_calling_action_reaches_the_point() {
    let rig = rig();
    let forked = tool(&rig);
    // The `Tool::run` path passes no action.
    let output = forked.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &Recorder::default(),
    );
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    // The `run_asking` path passes this call's action.
    let action = ActionId("a_0123456789abcdef".into());
    let output = forked.run_asking(
        &arguments(),
        &fakes::CancelToken::new(),
        &Recorder::default(),
        &FakeAsk {
            action: action.clone(),
        },
    );
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let seen = rig.seen_actions.lock().unwrap();
    assert_eq!(*seen, vec![None, Some(action)]);
    rig.registry.stop_delegates();
}

#[test]
fn the_start_writes_the_point_model_verbatim_with_its_point() {
    let rig = rig();
    let forked = tool(&rig);
    let output = forked.run(
        &arguments(),
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
    assert_eq!(started.tool.as_deref(), Some("delegate_fork"));
    // The point's model reference, verbatim: the start resolves nothing.
    assert_eq!(delegate.model, "fiber:fake/m:high");
    assert_eq!(delegate.forked_from, Some(forking().point));
    let launched = Deadline::after(DEADLINE)
        .recv(&rig.launched)
        .expect("the launcher ran");
    assert_eq!(launched.job_id, started.job_id);
    assert_eq!(launched.session_id, delegate.delegate_session_id);
    assert_eq!(launched.model, "fiber:fake/m:high");
    assert_eq!(rig.registry.stop_delegates(), 1);
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

#[test]
fn the_schema_names_only_description_prompt_and_timeout_ms() {
    let rig = rig();
    let definition = tool(&rig).definition();
    assert_eq!(definition.name, "delegate_fork");
    let properties = definition.input_schema["properties"]
        .as_object()
        .expect("an object schema");
    let mut keys: Vec<_> = properties.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["description", "prompt", "timeout_ms"]);
    assert_eq!(
        definition.input_schema["required"],
        serde_json::json!(["description", "prompt"])
    );
    assert!(
        !properties.contains_key("isolation"),
        "no isolation: it moves to #544"
    );
    assert!(!properties.contains_key("model"), "a fork takes no model");
    // The optional `timeout_ms` keeps the schema out of the strict
    // subset, so it is sent `strict: false`.
    let required: Vec<String> =
        serde_json::from_value(definition.input_schema["required"].clone()).unwrap();
    assert_ne!(keys, required);
    assert!(tool(&rig).effects(&arguments()).unwrap().always_reviewed);
}
