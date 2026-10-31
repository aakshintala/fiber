//! Tests for the extension-owned delegate stream: the `run` item an
//! extension binds with `host.delegate`, held finishes, timeouts and
//! failures.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{
    DelegateFinished, Event, ExtensionUi, FinishedWorktree, JobCompleted, Outcome, Ui,
};
use contract::shapes::{Choice, Failure, Question, Tokens, Usage, Worktree};
use contract::{ErrorCode, JobId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, Value};

use super::super::LuaExtension;
use super::super::Target;
use super::super::declared::CallbackTimeouts;
use super::super::hub::{Hub, Phase, StopReason};
use super::run_arg;

#[allow(clippy::too_many_lines, reason = "test helpers read top to bottom")]
/// A held finish with every optional field populated, so the `run`
/// table test pins each one; hook bookkeeping rides along to prove the
/// table leaves it out.
fn finished(job_id: &JobId) -> DelegateFinished {
    DelegateFinished {
        job_id: job_id.clone(),
        text: "Done.".to_owned(),
        artifact: Some("artifact.txt".to_owned()),
        questions: Some(vec![Question {
            header: "Deploy?".to_owned(),
            question: "Ship it?".to_owned(),
            options: vec![Choice {
                label: "yes".to_owned(),
                description: None,
            }],
            multi_select: None,
        }]),
        usage: Usage {
            tokens: Tokens {
                input: 1,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 2,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        changed_by: Some(vec!["redactor".to_owned()]),
        worktree: Some(FinishedWorktree {
            worktree: Worktree {
                path: "/w".to_owned(),
                branch: "scan".to_owned(),
                repository: None,
            },
            dirty: true,
        }),
    }
}

fn completed(job_id: &JobId, status: Outcome) -> JobCompleted {
    JobCompleted {
        job_id: job_id.clone(),
        status,
        error: None,
        process: None,
        output_tail: None,
        notice: None,
        changed_by: None,
        protected_paths: None,
    }
}

fn id(name: &str) -> JobId {
    JobId(name.to_owned())
}

/// A held finish reaches the `run` as one table carrying the finish's
/// fields under their `docs/events.md` key names, and nothing else: hook
/// bookkeeping such as `changed_by` stays out. A `failed` end with a
/// held finish still carries the finish fields. Without a held finish the
/// table carries only `job_id` and `status`.
#[test]
fn run_arg_carries_the_finish_when_held() {
    let job_id = id("j_1");
    let finish = finished(&job_id);
    let with_finish = run_arg(&completed(&job_id, Outcome::Completed), Some(&finish));
    let table = with_finish.as_object().expect("the run arg is a table");
    let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "artifact",
            "job_id",
            "questions",
            "status",
            "text",
            "usage",
            "worktree"
        ],
        "exactly the N7 keys, in {with_finish:?}"
    );
    assert_eq!(table["job_id"], "j_1");
    assert_eq!(table["status"], "completed");
    assert_eq!(table["text"], "Done.");
    assert_eq!(table["artifact"], "artifact.txt");
    assert_eq!(
        table["questions"],
        serde_json::to_value(&finish.questions).expect("questions serialize")
    );
    assert_eq!(
        table["usage"],
        serde_json::to_value(&finish.usage).expect("usage serializes")
    );
    assert_eq!(
        table["worktree"],
        serde_json::to_value(&finish.worktree).expect("worktree serializes")
    );
    assert_eq!(table["questions"][0]["header"], "Deploy?");
    assert_eq!(table["usage"]["tokens"]["input"], 1);
    assert_eq!(table["worktree"]["branch"], "scan");
    // A failed end with a held finish still carries the finish fields.
    let failed = run_arg(&completed(&job_id, Outcome::Failed), Some(&finish));
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["text"], "Done.");
    assert_eq!(failed["usage"], table["usage"]);
    let without_finish = run_arg(&completed(&job_id, Outcome::Completed), None);
    assert_eq!(
        without_finish,
        serde_json::json!({
            "job_id": "j_1",
            "status": "completed",
        })
    );
    for (status, name) in [
        (Outcome::Failed, "failed"),
        (Outcome::Cancelled, "cancelled"),
    ] {
        assert_eq!(
            run_arg(&completed(&id("j_9"), status), None),
            serde_json::json!({
                "job_id": "j_9",
                "status": name,
            })
        );
    }
}

/// A `delegate_finished` no `job_completed` follows pushes nothing; the
/// `job_completed` then pushes exactly one `run` item, and a second
/// completion for the same id pushes nothing more.
#[test]
fn hold_then_completion_pushes_exactly_one_item() {
    let hub = Hub::new(FakeClock::new());
    hub.lock().phase = Phase::Ready(Default::default());
    let job_id = id("j_1");
    hub.bind_delegate(job_id.clone(), 7, Duration::from_secs(5));
    assert!(
        hub.hold_delegate_finish(&finished(&job_id)),
        "the bound finish is held"
    );
    {
        let shared = hub.lock();
        assert!(
            !shared
                .queue
                .iter()
                .any(|job| matches!(job.target, Target::Delegate { .. })),
            "a held finish queues no run"
        );
    }
    assert!(
        hub.push_delegate_run("ext", &completed(&job_id, Outcome::Completed)),
        "the completion pushes the run"
    );
    {
        let shared = hub.lock();
        let items: Vec<_> = shared
            .queue
            .iter()
            .filter_map(|job| {
                if let Target::Delegate {
                    extension,
                    job_id,
                    handle,
                    timeout,
                } = &job.target
                {
                    Some((extension.clone(), job_id.clone(), *handle, *timeout))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            items,
            vec![("ext".to_owned(), job_id.clone(), 7, Duration::from_secs(5))],
            "exactly one delegate item, carrying the yield's handle and timeout"
        );
    }
    assert!(
        !hub.push_delegate_run("ext", &completed(&job_id, Outcome::Completed)),
        "the binding is consumed: a second completion pushes nothing"
    );
}

/// A `job_completed` no extension binds pushes nothing: released, removed
/// and model-owned ids never run.
#[test]
fn completion_without_a_binding_pushes_nothing() {
    let hub = Hub::new(FakeClock::new());
    hub.lock().phase = Phase::Ready(Default::default());
    assert!(
        !hub.push_delegate_run("ext", &completed(&id("j_1"), Outcome::Completed)),
        "nothing is bound"
    );
    assert!(hub.lock().queue.is_empty(), "nothing is queued");
}

/// A completion that lands after the stop pushes nothing.
#[test]
fn completion_after_stop_pushes_nothing() {
    let hub = Hub::new(FakeClock::new());
    let job_id = id("j_1");
    hub.bind_delegate(job_id.clone(), 7, Duration::from_secs(5));
    hub.lock().phase = Phase::Stopped(StopReason::Stopped);
    assert!(
        !hub.push_delegate_run("ext", &completed(&job_id, Outcome::Completed)),
        "the stopped extension queues no run"
    );
}

/// `release_delegates` forgets bindings, held finishes and queued items for
/// the released ids, and only those: a released id's completion pushes
/// nothing, a still-bound one still runs.
#[test]
fn release_forgets_only_the_released_ids() {
    let hub = Hub::new(FakeClock::new());
    hub.lock().phase = Phase::Ready(Default::default());
    let released = id("j_1");
    let kept = id("j_2");
    for job_id in [&released, &kept] {
        hub.bind_delegate(job_id.clone(), 7, Duration::from_secs(5));
        assert!(hub.hold_delegate_finish(&finished(job_id)));
    }
    assert!(hub.push_delegate_run("ext", &completed(&released, Outcome::Completed)));
    hub.release_delegates(std::slice::from_ref(&released));
    {
        let shared = hub.lock();
        assert!(
            !shared.queue.iter().any(
                |job| matches!(&job.target, Target::Delegate { job_id, .. } if job_id == &released)
            ),
            "the released id's queued item is dropped"
        );
    }
    assert!(
        !hub.push_delegate_run("ext", &completed(&released, Outcome::Completed)),
        "the released id's completion pushes nothing"
    );
    assert!(
        hub.push_delegate_run("ext", &completed(&kept, Outcome::Completed)),
        "the still-bound id still runs"
    );
}

/// The timeout a `run` item settles on is the per-call timeout from the
/// yield, even when it differs from every registered callback's.
#[test]
fn timeout_comes_from_the_yield_not_the_registry() {
    let target = Target::Delegate {
        extension: "ext".to_owned(),
        job_id: id("j_1"),
        handle: 7,
        timeout: Duration::from_secs(5),
    };
    assert_eq!(
        CallbackTimeouts::default().timeout(&target),
        Some(Duration::from_secs(5)),
        "the carried per-call timeout wins"
    );
}

// The rig below runs a real extension whose command starts a delegate
// through a fake starter, like `host::delegate_tests` does, then ends the
// job through the hub so the bound `run` executes on the thread.

/// A starter that binds every id: a `hold` spec waits for the test's
/// release, anything else answers at once.
struct Answering {
    next: AtomicU64,
    bound_tx: mpsc::Sender<JobId>,
    held_arrived_tx: mpsc::Sender<JobId>,
    held_release: Mutex<mpsc::Receiver<()>>,
}

impl contract::extension::HostDelegate for Answering {
    fn start(
        &self,
        _extension: &str,
        arguments: &Map<String, Value>,
        bind: &mut dyn FnMut(&JobId),
    ) -> Result<JobId, Failure> {
        let id = JobId(format!(
            "j_test_{}",
            self.next.fetch_add(1, Ordering::SeqCst)
        ));
        bind(&id);
        self.bound_tx.send(id.clone()).unwrap_or(());
        if arguments
            .get("description")
            .is_some_and(|description| description == "hold")
        {
            self.held_arrived_tx.send(id.clone()).unwrap_or(());
            match self
                .held_release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
            {
                Ok(()) | Err(mpsc::RecvError) => {}
            }
        }
        Ok(id)
    }

    fn retain(&self, _loaded: &[String]) -> usize {
        0
    }

    fn cancel(&self, _jobs: &[JobId]) -> usize {
        0
    }
}

/// An emitter that records every event for the test and forwards it to
/// the test's channel, so the test waits on each signal with a deadline
/// instead of sleeping.
struct Recording {
    events: Mutex<Vec<Event>>,
    tx: mpsc::Sender<Event>,
}

impl Recording {
    fn notices(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn statuses(&self) -> Vec<String> {
        self.notices()
            .iter()
            .filter_map(|event| {
                if let Event::ExtensionUi(ExtensionUi {
                    ui: Ui::Status { status },
                    ..
                }) = event
                {
                    Some(status.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

/// The `hook_failed` notices in `emit`.
fn hook_failed_notices(emit: &Recording) -> Vec<contract::events::Notice> {
    emit.notices()
        .iter()
        .filter_map(|event| {
            if let Event::Notice(notice) = event
                && notice.code == ErrorCode::HookFailed
            {
                Some(notice.clone())
            } else {
                None
            }
        })
        .collect()
}

impl contract::emit::Emit for Recording {
    fn emit(&self, event: &Event) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
        self.tx.send(event.clone()).unwrap_or(());
    }
}

/// A running extension whose `go` command starts one delegate whose `run`
/// body is `run_body`, with per-call timeout `run_timeout_ms`. The temp dir
/// is returned so `init.lua` stays on disk while the extension runs.
/// The pieces a serve-level test holds: the dir keeps `init.lua` alive.
struct Rig {
    _dir: fakes::TempDir,
    ext: Arc<LuaExtension>,
    emit: Arc<Recording>,
    bound_rx: mpsc::Receiver<JobId>,
    event_rx: mpsc::Receiver<Event>,
    held_arrived_rx: mpsc::Receiver<JobId>,
    held_release_tx: mpsc::Sender<()>,
}

fn running_extension_inner(
    run_body: &str,
    run_timeout_ms: u64,
    hook_body: &str,
    clock: Arc<FakeClock>,
) -> Rig {
    let dir = fakes::TempDir::new("fiber-delegates-run");
    std::fs::write(
        dir.path().join("init.lua"),
        format!(
            "fiber.command(\"go\", {{ timeout = 10000, run = function()\n\
               return host.delegate({{ description = \"scan\" }},\n\
                 {{ timeout = {run_timeout_ms}, run = function(done)\n\
                   {run_body}\n\
                 end }})\n\
             end }})\n\
             fiber.hook(\"turn_start\", {{ timeout = 120000, on_failure = \"non-blocking\",\n\
               run = function(call)\n{hook_body}\n\
               end }})\n"
        ),
    )
    .unwrap();
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/delegates",
        dir.path(),
        PathBuf::from("/nonexistent-fiber-home"),
        clock,
    ));
    // A `host.delegate` start before the inbox fails closed: the tests
    // below start delegates, so the inbox is wired like the session's
    // loop would.
    let (inbox_tx, _inbox_rx) = mpsc::channel();
    ext.deliver_to(inbox_tx);
    let (event_tx, event_rx) = mpsc::channel();
    let emit = Arc::new(Recording {
        events: Mutex::new(Vec::new()),
        tx: event_tx,
    });
    ext.set_emit(Arc::clone(&emit) as Arc<dyn contract::emit::Emit>);
    let (bound_tx, bound_rx) = mpsc::channel();
    let (held_arrived_tx, held_arrived_rx) = mpsc::channel();
    let (held_release_tx, held_release_rx) = mpsc::channel();
    ext.set_delegate(Arc::new(Answering {
        next: AtomicU64::new(0),
        bound_tx,
        held_arrived_tx,
        held_release: Mutex::new(held_release_rx),
    }));
    Rig {
        _dir: dir,
        ext,
        emit,
        bound_rx,
        event_rx,
        held_arrived_rx,
        held_release_tx,
    }
}

/// A running extension whose `go` command starts one delegate whose `run`
/// body is `run_body`, with per-call timeout `run_timeout_ms`. Its hook
/// never fires in these tests.
fn running_extension(run_body: &str, run_timeout_ms: u64, clock: Arc<FakeClock>) -> Rig {
    running_extension_inner(run_body, run_timeout_ms, "return {}", clock)
}

/// Starts one delegate through `go` and returns its bound id.
fn start_job(ext: &Arc<LuaExtension>, bound_rx: &mpsc::Receiver<JobId>) -> JobId {
    let go = Arc::clone(ext);
    let started = fakes::within(
        "go",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || go.command("go", ""),
    );
    let job_id = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(bound_rx)
        .expect("waited for the starter's bind");
    assert_eq!(started.unwrap(), job_id.0.clone());
    job_id
}

/// Ends `job_id` on `ext` through the hub: the held finish, then the
/// completion, as the log fan-out would offer them.
fn end_job(ext: &LuaExtension, job_id: &JobId, status: Outcome) -> bool {
    assert!(
        ext.hub.hold_delegate_finish(&finished(job_id)),
        "the starter bound the id, so the finish is held"
    );
    ext.hub
        .push_delegate_run(ext.name(), &completed(job_id, status))
}

/// Waits until the extension thread has no queued or running call left.
fn wait_until_idle(ext: &LuaExtension) {
    assert!(
        ext.hub
            .wait_for(ext.hub.lock(), MUST_SUCCEED_WITHIN, |shared| shared
                .queue
                .is_empty()
                && shared.calls.is_empty()),
        "the extension thread drains the delegate item"
    );
}

/// A completed run receives `job_id`, `status`, `text`, `artifact`,
/// `questions`, `usage` and `worktree`, and its return is ignored: no
/// notice follows.
#[test]
fn completed_run_receives_the_finish_fields() {
    let clock = FakeClock::new();
    let rig = running_extension(
        "host.status(done.job_id .. \"/\" .. done.status .. \"/\" .. tostring(done.text) .. \"/\" .. tostring(done.artifact))\n\
         host.status(\"usage:\" .. done.usage.tokens.input .. \"/\" .. done.usage.tokens.output .. \"/\" .. tostring(done.usage.cost))\n\
         host.status(\"q:\" .. #done.questions .. \"/\" .. done.questions[1].header)\n\
         host.status(\"wt:\" .. done.worktree.path .. \"/\" .. done.worktree.branch .. \"/\" .. tostring(done.worktree.dirty))",
        5_000,
        clock,
    );
    let ext = &rig.ext;
    let job_id = start_job(ext, &rig.bound_rx);
    assert!(
        end_job(ext, &job_id, Outcome::Completed),
        "the completion pushes the run"
    );
    wait_until_idle(ext);
    assert!(
        rig.emit
            .statuses()
            .iter()
            .any(|status| status == &format!("{}/{}/Done./artifact.txt", job_id.0, "completed")),
        "the run saw job_id, status, text and artifact, in {:?}",
        rig.emit.statuses()
    );
    for expected in ["usage:1/2/0.0", "q:1/Deploy?", "wt:/w/scan/true"] {
        assert!(
            rig.emit.statuses().iter().any(|status| status == expected),
            "the run saw {expected}, in {:?}",
            rig.emit.statuses()
        );
    }
    assert!(
        rig.emit.notices().iter().all(|event| !matches!(
            event,
            Event::Notice(notice) if notice.code == ErrorCode::HookFailed
        )),
        "a run that returns emits no failure notice"
    );
}

/// A `run` still parked past its per-call timeout ends there: the session
/// sees the R4 `hook_failed` notice naming the extension, and a second
/// completion for the same id pushes nothing.
#[test]
fn timed_out_run_emits_hook_failed_naming_the_extension() {
    let clock = FakeClock::new();
    let timeout = Duration::from_secs(2);
    let rig = running_extension(
        "host.status(\"run-started\")\n\
         host.delegate({ description = \"hold\" }, { timeout = 120000, run = function() end })",
        2_000,
        Arc::clone(&clock),
    );
    let ext = &rig.ext;
    let job_id = start_job(ext, &rig.bound_rx);
    let pushed_at = clock.now();
    assert!(
        end_job(ext, &job_id, Outcome::Completed),
        "the completion pushes the run"
    );
    let run_started = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rig.event_rx)
        .expect("waited for the run to park");
    assert!(
        matches!(
            run_started,
            Event::ExtensionUi(ExtensionUi {
                ui: Ui::Status { ref status },
                ..
            }) if status == "run-started"
        ),
        "the run parks past its timeout, in {run_started:?}"
    );
    // The park lands before the spawn reaches the holding fake: the run
    // is parked on its own deadline either way.
    clock.advance_when_parked(
        Some(pushed_at + timeout),
        None,
        timeout,
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    wait_until_idle(ext);
    let hook_failed = hook_failed_notices(&rig.emit);
    assert_eq!(
        hook_failed.len(),
        1,
        "one failure notice, in {:?}",
        rig.emit.notices()
    );
    assert_eq!(
        hook_failed[0].extension.as_deref(),
        Some(ext.name()),
        "the notice names the extension"
    );
    assert!(
        hook_failed[0].message.contains("2000 ms"),
        "the notice carries the per-call timeout, not a registry default: {}",
        hook_failed[0].message
    );
    assert!(
        !ext.hub
            .push_delegate_run(ext.name(), &completed(&job_id, Outcome::Completed)),
        "the teardown consumed the binding: a second completion pushes nothing"
    );
    // Releases the run's held inner start, whose late answer is dropped.
    rig.held_release_tx.send(()).unwrap_or(());
}

/// A `run` that raises ends the same way: the R4 `hook_failed` notice
/// naming the extension.
#[test]
fn raised_run_emits_hook_failed_naming_the_extension() {
    let clock = FakeClock::new();
    let rig = running_extension("error(\"boom-\" .. done.job_id)", 5_000, clock);
    let ext = &rig.ext;
    let job_id = start_job(ext, &rig.bound_rx);
    assert!(
        end_job(ext, &job_id, Outcome::Completed),
        "the completion pushes the run"
    );
    wait_until_idle(ext);
    let hook_failed = hook_failed_notices(&rig.emit);
    assert_eq!(
        hook_failed.len(),
        1,
        "one failure notice, in {:?}",
        rig.emit.notices()
    );
    assert_eq!(
        hook_failed[0].extension.as_deref(),
        Some(ext.name()),
        "the notice names the extension"
    );
    assert!(
        hook_failed[0]
            .message
            .contains(&format!("boom-{}", job_id.0)),
        "the notice carries the raise: {}",
        hook_failed[0].message
    );
}

/// A hook parked on its own delegate start holds the delegate stream: the
/// pushed `run` queues but does not start until the hook's start answers
/// and the hook finishes.
#[test]
fn parked_hook_holds_the_delegate_item_until_it_finishes() {
    let clock = FakeClock::new();
    let rig = running_extension_inner(
        "host.status(\"run-\" .. done.job_id)",
        5_000,
        "host.status(\"hook-started\")\n\
         host.delegate({ description = \"hold\" }, { timeout = 120000, run = function() end })\n\
         return {}",
        Arc::clone(&clock),
    );
    let ext = &rig.ext;
    let job_id = start_job(ext, &rig.bound_rx);
    let hook = Arc::clone(ext);
    let (hook_tx, hook_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("delegates-hook-caller".to_owned())
        .spawn(move || hook_tx.send(hook.hook("turn_start", 0, serde_json::Value::Null)))
        .expect("the hook caller spawns");
    let started = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rig.event_rx)
        .expect("waited for the hook to start");
    assert!(
        matches!(
            started,
            Event::ExtensionUi(ExtensionUi {
                ui: Ui::Status { ref status },
                ..
            }) if status == "hook-started"
        ),
        "the hook runs first, in {started:?}"
    );
    // The spawn runs after the worker parks on it, so the hook's arrival
    // at the holding fake proves the hook is parked, not just running.
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rig.held_arrived_rx)
        .expect("waited for the hook's start to park");
    assert!(
        end_job(ext, &job_id, Outcome::Completed),
        "the completion pushes the run behind the parked hook"
    );
    {
        let shared = ext.hub.lock();
        assert!(
            shared.queue.iter().any(
                |job| matches!(&job.target, Target::Delegate { job_id: queued, .. } if queued == &job_id)
            ),
            "the run queues behind the parked hook"
        );
    }
    assert!(
        rig.event_rx.try_recv().is_err(),
        "the run does not start while the hook is parked"
    );
    // Answering the hook's start resumes the hook, whose finish frees
    // the stream for the queued run.
    rig.held_release_tx.send(()).unwrap_or(());
    let ran = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rig.event_rx)
        .expect("waited for the released run");
    assert!(
        matches!(
            ran,
            Event::ExtensionUi(ExtensionUi {
                ui: Ui::Status { ref status },
                ..
            }) if status == &format!("run-{}", job_id.0)
        ),
        "the hook's end releases the run, in {ran:?}"
    );
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&hook_rx)
            .expect("waited for the hook's answer")
            .is_ok(),
        "the parked hook answers once its start does"
    );
    wait_until_idle(ext);
}

/// A settled delegate `run` clears its handle from the VM: a replay of
/// the consumed handle runs nothing, so a stale completion cannot
/// resurrect the old `run`.
#[test]
fn a_settled_run_clears_the_run_handle_so_a_replay_runs_nothing() {
    let clock = FakeClock::new();
    let rig = running_extension(
        "host.status(\"run-\" .. done.job_id)",
        5_000,
        Arc::clone(&clock),
    );
    let ext = &rig.ext;
    let job_id = start_job(ext, &rig.bound_rx);
    // The binding carries the chunk-local handle the replay forges.
    let (handle, timeout) = ext
        .hub
        .lock()
        .delegates
        .get(&job_id)
        .copied()
        .expect("the start bound its handle");
    assert!(
        end_job(ext, &job_id, Outcome::Completed),
        "the completion pushes the run"
    );
    wait_until_idle(ext);
    let runs = || {
        rig.emit
            .statuses()
            .iter()
            .filter(|status| *status == &format!("run-{}", job_id.0))
            .count()
    };
    assert_eq!(runs(), 1, "the run ran once");
    assert!(
        !ext.hub
            .push_delegate_run(ext.name(), &completed(&job_id, Outcome::Completed)),
        "a second completion for the same id pushes nothing"
    );
    // A replay of the consumed handle: the settle cleared the VM entry,
    // so the thread drops it instead of running the old `run` again.
    {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Delegate {
                extension: ext.name().to_owned(),
                job_id: id("j_replay"),
                handle,
                timeout,
            },
            run_arg(
                &completed(&job_id, Outcome::Completed),
                Some(&finished(&job_id)),
            ),
            clock.now(),
        );
        drop(shared);
        ext.hub.notify();
    }
    wait_until_idle(ext);
    assert_eq!(
        runs(),
        1,
        "the replay ran nothing: the settled run cleared its handle"
    );
}
