//! `host.delegate` through the public API (`docs/extensions.md`, "Host calls"
//! and "The delegate run call"): commands start two delegates, the session
//! releases one's id, and only the still-bound completion runs its `run`.

mod common;

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use common::{Setup, install_lua};
use config::{Config, ProjectKey, Sources};
use contract::events::{Event, ExtensionLog, JobCompleted, Outcome};
use contract::extension::{ExtensionDoor, HostDelegate};
use contract::hook::Hooks;
use contract::inbox::Delivery;
use contract::shapes::Failure;
use contract::{CommandId, JobId};
use extensions::SessionExtensions;
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};
use serde_json::{Map, json};

/// How long a test waits for one delivery before failing.
const WAIT: Duration = Duration::from_secs(5);

/// How long a test waits for a log that must never arrive.
const QUIET: Duration = Duration::from_millis(200);

/// A [`HostDelegate`] handing out queued ids, recording every start.
struct Fake {
    next: Mutex<Vec<JobId>>,
    started: Mutex<Vec<(String, Map<String, serde_json::Value>)>>,
}

impl Fake {
    fn new(ids: &[&str]) -> Self {
        Self {
            next: Mutex::new(ids.iter().map(|id| JobId((*id).into())).collect()),
            started: Mutex::new(Vec::new()),
        }
    }
}

#[allow(
    clippy::unwrap_used,
    reason = "a test fake; a poisoned lock is the test's failure"
)]
impl HostDelegate for Fake {
    fn start(
        &self,
        extension: &str,
        arguments: &Map<String, serde_json::Value>,
        bind: &mut dyn FnMut(&JobId),
    ) -> Result<JobId, Failure> {
        let id = self.next.lock().unwrap().remove(0);
        bind(&id);
        self.started
            .lock()
            .unwrap()
            .push((extension.to_owned(), arguments.clone()));
        Ok(id)
    }

    fn retain(&self, _loaded: &[String]) -> usize {
        0
    }

    fn cancel(&self, _jobs: &[JobId]) -> usize {
        0
    }
}

#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn load(setup: &Setup, clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .expect("the test home loads");
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
    });
    Arc::new(
        Deadline::after(WAIT)
            .recv(&rx)
            .expect("waited for the extensions to load"),
    )
}

/// Runs the extension command `name` through the session door: the waiter
/// thread drops the answer, so what the callback did is observed on the
/// inbox.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn run(session: &Arc<SessionExtensions>, name: &str) {
    let release = session
        .command(&CommandId("c_test".to_owned()), name, "")
        .expect("the command is admitted");
    release();
}

/// The next `extension_log` line on `inbox`.
#[allow(clippy::panic, reason = "a test helper; a missing line is the test's")]
#[track_caller]
fn next_log(inbox: &mpsc::Receiver<Delivery>, what: &str) -> String {
    match Deadline::after(WAIT).recv(inbox) {
        Ok(Delivery::ExtensionLog(ExtensionLog { message, .. })) => message,
        Ok(other) => panic!("expected {what}, got {other:?}"),
        Err(_) => panic!("waited {WAIT:?} for {what}"),
    }
}

/// A `job_completed` event for `id` with `status`.
fn completed(id: &str, status: Outcome) -> Event {
    Event::JobCompleted(JobCompleted {
        job_id: JobId(id.into()),
        status,
        error: None,
        process: None,
        output_tail: None,

        notice: None,
        changed_by: None,
    })
}

/// The job id logged after `started:`.
#[allow(
    clippy::expect_used,
    reason = "a test helper; a missing id is the test's"
)]
#[track_caller]
fn started_id(log: &str) -> String {
    log.strip_prefix("started:")
        .expect("the command logged its delegate id")
        .to_owned()
}

const DELEGATE_INIT: &str = r#"
local function start(tag)
  local id = host.delegate(
    { description = tag, prompt = "p", model = "deep" },
    { timeout = 5000, run = function(finish)
        host.log("ran:" .. finish.job_id .. ":" .. finish.status)
      end })
  host.log("started:" .. id)
  return id
end
fiber.command("go1", { timeout = 5000, run = function() return start("one") end })
fiber.command("go2", { timeout = 5000, run = function() return start("two") end })
"#;

#[test]
fn a_released_completion_queues_nothing_while_a_bound_one_runs_its_run() {
    let setup = Setup::new();
    install_lua(&setup, "d", &json!({}), DELEGATE_INIT);
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let fake = Arc::new(Fake::new(&["j_1", "j_2"]));
    session.delegate_to(Arc::clone(&fake) as Arc<dyn HostDelegate>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    run(&session, "go1");
    run(&session, "go2");
    // Both delegates bound, in start order: the logs name the actual ids.
    let first = started_id(&next_log(&inbox_rx, "the first delegate start"));
    let second = started_id(&next_log(&inbox_rx, "the second delegate start"));
    assert_eq!(fake.started.lock().unwrap().len(), 2);
    session.release_delegates(&[JobId(first.clone())]);
    session.deliver(&completed(&first, Outcome::Cancelled), None);
    session.deliver(&completed(&second, Outcome::Completed), None);
    // The released completion queues nothing: the first log after both
    // completions is the still-bound run, in queue order.
    assert_eq!(
        next_log(&inbox_rx, "the bound run"),
        format!("ran:{second}:completed")
    );
    // Nothing else follows: the released run never logged.
    assert!(
        Deadline::after(QUIET).recv(&inbox_rx).is_err(),
        "no released run logged"
    );
}

/// A delegate finish for `id`, as the session end writes it: durable, so
/// the watcher fan-out always queues it.
fn finished(id: &str) -> Event {
    Event::DelegateFinished(contract::events::DelegateFinished {
        job_id: JobId(id.into()),
        text: "done".into(),
        artifact: None,
        questions: None,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        worktree: None,
        changed_by: None,
    })
}

const WATCHER_INIT: &str = r#"
fiber.watch({"delegate_finished"}, { timeout = 5000, run = function(ev)
    host.log("saw:" .. ev.payload.job_id)
  end })
"#;

#[test]
fn a_reloaded_watcher_receives_the_kept_running_delegate_finish_exactly_once() {
    let setup = Setup::new();
    let dir = install_lua(&setup, "d", &json!({}), DELEGATE_INIT);
    let clock = FakeClock::new();
    let first = load(&setup, Arc::clone(&clock));
    let fake = Arc::new(Fake::new(&["j_1"]));
    first.delegate_to(Arc::clone(&fake) as Arc<dyn HostDelegate>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    first.deliver_to(inbox_tx);
    // One delegate binds in the first set's VM.
    run(&first, "go1");
    let id = started_id(&next_log(&inbox_rx, "the delegate start"));
    // A reload drops the first set with its binding and loads a second
    // set whose watcher registered before the finish below.
    std::fs::write(dir.join("init.lua"), WATCHER_INIT).unwrap();
    let second = load(&setup, clock);
    let (inbox2_tx, inbox2_rx) = mpsc::channel();
    second.deliver_to(inbox2_tx);
    drop(first);
    second.deliver(&finished(&id), None);
    second.deliver(&completed(&id, Outcome::Completed), None);
    // The new VM never bound the id, so the queued item is the watcher's:
    // it ran exactly once, and the old run never logged.
    assert_eq!(
        next_log(&inbox2_rx, "the reloaded watcher"),
        format!("saw:{id}")
    );
    assert!(
        Deadline::after(QUIET).recv(&inbox2_rx).is_err(),
        "the watcher ran exactly once"
    );
    assert!(
        Deadline::after(QUIET).recv(&inbox_rx).is_err(),
        "the dropped set ran no old run"
    );
}
