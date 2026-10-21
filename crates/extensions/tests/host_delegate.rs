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
