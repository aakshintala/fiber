//! Tests first for `delegate_fork`: its order, depth, cap boundaries,
//! point action and schema.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use contract::inbox::Delivery;
use contract::jobs::{JobRecord, Jobs as _};
use contract::shapes::{ContentPart, Failure, Point};
use contract::tool::{Answered, Ask, Asking, Tool as _};
use contract::{ActionId, Seq};
use fakes::Deadline;
use fakes::Recorder;

use super::super::run::{Fresh, Start};
use super::super::test_rig::{DEADLINE, Rig, child_group, rig};
use super::super::{DelegateFork, ForkPoint, Forking};

fn forking() -> Forking {
    Forking {
        point: Point {
            session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".into()),
            seq: Seq(41),
            project: None,
        },
        model: "fiber:fake/m:high".to_owned(),
    }
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
        wiring: rig.wiring(),
        point,
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
    forked.wiring.depth = 2;
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
        wiring: rig.wiring(),
        resolve: super::super::models::Resolve {
            lookup: Arc::new(|model: &str| match model {
                "fiber:fake/m" => Ok("fake/m".to_owned()),
                _ => Err(format!("Unknown delegate model {model:?}.")),
            }),
            catalogue: Arc::from("References:\nfiber:fake/m"),
            credential: Arc::new(|_| Ok(None)),
        },
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
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
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
fn malformed_isolation_fails_before_reading_the_point() {
    let rig = rig();
    let forked = tool(&rig);
    for value in [
        serde_json::json!(1),
        serde_json::json!(null),
        serde_json::json!("sometimes"),
    ] {
        let mut args = arguments();
        args.insert("isolation".into(), value);
        let output = forked.run(&args, &fakes::CancelToken::new(), &Recorder::default());
        assert_eq!(
            output.error.unwrap().code,
            contract::ErrorCode::InvalidArguments
        );
        assert!(output.jobs.is_empty());
    }
    assert_eq!(rig.point_calls.load(Ordering::SeqCst), 0);
    assert_eq!(rig.launch_count.load(Ordering::SeqCst), 0);
}

#[test]
fn isolation_creates_on_the_parent_workspace() {
    let rig = rig();
    let mut forked = tool(&rig);
    let workspace = forked.wiring.workspace.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    forked.wiring.create = Arc::new(
        #[allow(
            clippy::result_large_err,
            reason = "the jobs boundary returns its typed error by value"
        )]
        move |workspace, id| {
            tx.send((workspace.to_path_buf(), id.clone())).unwrap();
            Ok((
                super::super::EndWorktree {
                    path: workspace.join(&id.0),
                    branch: format!("fiber/{}", id.0),
                    base: Some("base".into()),
                    remove: true,
                },
                super::super::WorktreeGuard::new(|| {}),
            ))
        },
    );
    let mut args = arguments();
    args.insert("isolation".into(), serde_json::json!("worktree"));
    let output = forked.run(&args, &fakes::CancelToken::new(), &Recorder::default());
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none(), "{:?}", output.error);
    let launched = Deadline::after(DEADLINE).recv_or_fail(&rig.launched, "the fork launches");
    let (created_in, id) =
        Deadline::after(DEADLINE).recv_or_fail(&rx, "the fork creates its worktree");
    assert_eq!(created_in, workspace);
    assert_eq!(id, launched.session_id);
    let worktree = launched.worktree.unwrap();
    assert_eq!(worktree.path, workspace.join(&id.0).to_string_lossy());
    assert_eq!(worktree.branch, format!("fiber/{}", id.0));
    assert_eq!(launched.workspace, std::path::PathBuf::from(&worktree.path));
    let [_, JobRecord::DelegateStarted(delegate)] = &output.jobs[..] else {
        panic!("the fork records its start");
    };
    assert_eq!(delegate.worktree.as_ref(), Some(&worktree));
    rig.registry.stop_delegates();
}

#[test]
fn explicit_none_isolation_keeps_the_parent_workspace() {
    let rig = rig();
    let forked = tool(&rig);
    let mut args = arguments();
    args.insert("isolation".into(), serde_json::json!("none"));
    let output = forked.run(&args, &fakes::CancelToken::new(), &Recorder::default());
    let _watchdog = child_group(&rig);
    assert!(output.error.is_none());
    let launched = Deadline::after(DEADLINE).recv_or_fail(&rig.launched, "the fork launches");
    assert!(launched.worktree.is_none());
    assert_eq!(launched.workspace, forked.wiring.workspace);
    rig.registry.stop_delegates();
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
    let Start::Fresh(Fresh::Fork { from }) = &launched.start else {
        panic!("a fork launches with its point");
    };
    assert_eq!(from, &forking());
    // The receipt names the job and its log: deleting either half of the
    // text breaks this.
    let [ContentPart::Text { text }] = &output.content[..] else {
        panic!("a good call answers with its receipt: {:?}", output.content);
    };
    assert_eq!(
        text,
        &format!(
            "Started delegate {}. Its log is {}.",
            started.job_id.0,
            rig.sessions
                .join(&delegate.delegate_session_id.0)
                .join("events.jsonl")
                .to_string_lossy(),
        ),
    );
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
fn the_schema_names_only_description_prompt_isolation_and_timeout_ms() {
    let rig = rig();
    let definition = tool(&rig).definition();
    assert_eq!(definition.name, "delegate_fork");
    let properties = definition.input_schema["properties"]
        .as_object()
        .expect("an object schema");
    let mut keys: Vec<_> = properties.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["description", "isolation", "prompt", "timeout_ms"]);
    assert_eq!(
        definition.input_schema["required"],
        serde_json::json!(["description", "prompt"])
    );
    assert_eq!(
        properties["isolation"],
        serde_json::json!({
            "type": "string", "enum": ["none", "worktree"]
        })
    );
    assert_eq!(definition.input_schema["additionalProperties"], false);
    assert!(!properties.contains_key("model"), "a fork takes no model");
    // The optional `timeout_ms` keeps the schema out of the strict
    // subset, so it is sent `strict: false`.
    let required: Vec<String> =
        serde_json::from_value(definition.input_schema["required"].clone()).unwrap();
    assert_ne!(keys, required);
    assert!(tool(&rig).effects(&arguments()).unwrap().always_reviewed);
}
