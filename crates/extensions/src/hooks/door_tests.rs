//! `SessionExtensions::reply` across two extensions
//! (`docs/extensions.md`, "Commands and screens"): the extension holding
//! the ask takes the reply, in load order; any other reply is handed back
//! for the loop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::RequestId;
use contract::commands::{Reply, ReplyAnswer};
use contract::events::{InteractionRequested, ResolvedBy};
use contract::extension::ExtensionDoor;
use contract::inbox::{Ack, Delivery};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::json;

use super::super::SessionExtensions;
use crate::host::FakeLock;
use crate::{Origin, Request, plan};

/// Wall-clock bound on a wait for a delivery.
const WAIT: Duration = Duration::from_secs(5);

struct Home {
    root: fakes::TempDir,
    watchdog: Option<fakes::Watchdog>,
}

impl Drop for Home {
    fn drop(&mut self) {
        self.watchdog.take().unwrap().stand_down(WAIT);
    }
}

impl Home {
    fn new() -> Self {
        let watchdog = fakes::Watchdog::matching("fiber-door-reply");
        let root = fakes::TempDir::new("fiber-door-reply");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self {
            root,
            watchdog: Some(watchdog),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn install(&self, short: &str, init: &str) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("extension.json"),
            json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1})
                .to_string(),
        )
        .unwrap();
        fs::write(src.join("init.lua"), init).unwrap();
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    #[track_caller]
    fn load(&self) -> Arc<SessionExtensions> {
        self.load_clock(FakeClock::new())
    }

    fn load_clock(&self, clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap();
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, clock, locks, None)
        }))
    }
}

fn asking(short: &str) -> String {
    format!(
        "fiber.command(\"go\", {{ timeout = 5000, run = function()\n\
         local answer = host.ask(\"confirm\", {{ prompt = \"{short}?\" }})\n\
         return tostring(answer.confirmed) end }})\n"
    )
}

/// Runs `command` on `session`'s `which`-th extension and sends its result.
fn run(
    session: &Arc<SessionExtensions>,
    which: usize,
    command: &str,
) -> mpsc::Receiver<Result<String, crate::Error>> {
    let lua = Arc::clone(&session.lua[which]);
    let command = command.to_owned();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(lua.command(&command, ""));
    });
    done_rx
}

#[track_caller]
fn interaction(rx: &mpsc::Receiver<Delivery>, wait: &Deadline) -> InteractionRequested {
    let Delivery::Interaction(requested) = wait.recv(rx).expect("the Interaction arrives") else {
        panic!("an unexpected delivery arrives");
    };
    requested
}

/// Runs `f` on its own thread and waits for it under [`WAIT`], so a load
/// that never returns fails the test instead of hanging it.
#[track_caller]
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the extensions")
}

#[test]
fn the_extension_holding_the_ask_takes_the_reply_in_load_order() {
    let home = Home::new();
    home.install("a", &asking("a"));
    home.install("b", &asking("b"));
    let session = home.load();
    let (inbox_tx, rx) = mpsc::channel();
    for lua in &session.lua {
        lua.deliver_to(inbox_tx.clone());
    }
    session.answerable(true);
    // Both commands park on their asks; each Interaction names its own
    // extension, whatever order they arrive in.
    let first = run(&session, 0, "go");
    let second = run(&session, 1, "go");
    let mut ids = std::collections::BTreeMap::new();
    // One deadline for the whole wait: both Interactions arrive on one
    // stream with nothing between them but a map insert.
    let wait = Deadline::after(WAIT);
    for _ in 0..2 {
        let requested = interaction(&rx, &wait);
        ids.insert(
            requested.extension.clone().unwrap_or_default(),
            requested.request_id,
        );
    }
    let (id_a, id_b) = (
        ids.remove("fiber.test/a").expect("a asks"),
        ids.remove("fiber.test/b").expect("b asks"),
    );
    // A reply for B passes through A (which hands it back) to B.
    let (tx, accepted) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer.is_ok());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: id_b.clone(),
                    answer: ReplyAnswer::Confirmed { confirmed: true },
                },
                ack
            )
            .is_none(),
        "the holding extension takes the reply"
    );
    let Delivery::Resolved(resolved, ack) = Deadline::after(WAIT)
        .recv(&rx)
        .expect("the Resolved arrives")
    else {
        panic!("a Resolved arrives");
    };
    assert_eq!(resolved.request_id, id_b);
    assert_eq!(resolved.by, ResolvedBy::Person);
    (ack.0)(Ok(None));
    assert!(
        Deadline::after(WAIT)
            .recv(&accepted)
            .expect("the reply is answered"),
        "the reply is accepted once the line is in the log"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&second)
            .expect("the command returns")
            .unwrap(),
        "true"
    );
    // A's ask is still pending: a fitting reply resolves it too.
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: id_a,
                    answer: ReplyAnswer::Confirmed { confirmed: false },
                },
                ack
            )
            .is_none()
    );
    let Delivery::Resolved(_, ack) = Deadline::after(WAIT)
        .recv(&rx)
        .expect("the Resolved arrives")
    else {
        panic!("a Resolved arrives");
    };
    (ack.0)(Ok(None));
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&first)
            .expect("the command returns")
            .unwrap(),
        "false"
    );
}

fn tool_answer() -> Box<contract::events::ToolCallCompleted> {
    Box::new(contract::events::ToolCallCompleted {
        status: contract::events::CallStatus::Completed,
        content: vec![contract::shapes::ContentPart::Text {
            text: "answer".into(),
        }],
        reason: None,
        error: None,
        process: None,
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    })
}

#[test]
fn an_admitted_command_holds_its_runner_through_success_and_lua_error() {
    for failing in [false, true] {
        let home = Home::new();
        home.install("a", &format!("fiber.command('go', {{ timeout = 5000, run = function() local o = host.tool('read', {{path='x'}}) {} return o.status .. '|' .. o.content[1].text end }})", if failing { "error('boom')" } else { "" }));
        let session = home.load();
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let admitted = session
            .commands
            .admit_with("go", "", Some(&contract::CommandId("c_admitted".into())))
            .unwrap();
        assert_eq!(session.lua[0].runners_held(), 1);
        let queued = admitted.queued().clone();
        queued.release();
        let (send, result) = mpsc::channel();
        thread::spawn(move || {
            let _sent = send.send(queued.wait());
        });
        let Delivery::InnerCall(request) =
            Deadline::after(WAIT).recv_or_fail(&rx, "admitted command inner call")
        else {
            panic!("expected inner call");
        };
        assert_eq!(request.command.0, "c_admitted");
        assert_eq!(request.extension, "fiber.test/a");
        assert_eq!(request.name, "read");
        assert_eq!(request.arguments, json!({"path":"x"}));
        assert!(!request.cancel.is_cancelled());
        (request.taken)();
        (request.answer)(tool_answer());
        let returned = Deadline::after(WAIT).recv_or_fail(&result, "admitted command end");
        if failing {
            assert!(returned.unwrap_err().to_string().contains("boom"));
        } else {
            assert_eq!(returned.unwrap(), json!("completed|answer"));
        }
        assert_eq!(session.lua[0].runners_held(), 0);
        assert!(request.cancel.is_cancelled());
    }
}

#[test]
fn a_command_deadline_ends_its_runner_at_and_above_but_not_below_the_boundary() {
    for milliseconds in [1099, 1100, 1101] {
        let home = Home::new();
        home.install("a", "fiber.command('go', { timeout = 100, run = function() return host.tool('read', {}).status end })");
        let clock = FakeClock::new();
        let session = home.load_clock(clock.clone());
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let admitted = session
            .commands
            .admit_with("go", "", Some(&contract::CommandId("c_timeout".into())))
            .unwrap();
        let queued = admitted.queued().clone();
        queued.release();
        let (send, result) = mpsc::channel();
        let caller = thread::spawn(move || {
            let _sent = send.send(queued.wait());
        });
        let Delivery::InnerCall(request) =
            Deadline::after(WAIT).recv_or_fail(&rx, "parked command inner call")
        else {
            panic!("expected inner call");
        };
        (request.taken)();
        let until = clock.origin() + Duration::from_millis(1100);
        assert!(clock.await_thread_parked(caller.thread().id(), Some(until), WAIT));
        clock.advance(Duration::from_millis(milliseconds));
        if milliseconds < 1100 {
            assert!(!request.cancel.is_cancelled());
            assert_eq!(session.lua[0].runners_held(), 1);
            assert!(result.try_recv().is_err());
            assert!(clock.await_thread_parked(caller.thread().id(), Some(until), WAIT));
            clock.advance(Duration::from_millis(1));
        }
        assert!(matches!(
            Deadline::after(WAIT).recv_or_fail(&result, "command timeout"),
            Err(crate::Error::Timeout { .. })
        ));
        assert_eq!(session.lua[0].runners_held(), 0);
        assert!(request.cancel.is_cancelled());
        (request.answer)(tool_answer());
        assert_eq!(session.lua[0].runners_held(), 0);
    }
}

#[test]
fn a_reply_no_extension_holds_is_handed_back() {
    let home = Home::new();
    home.install("a", &asking("a"));
    let session = home.load();
    session.answerable(true);
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: RequestId("r_other".into()),
                    answer: ReplyAnswer::Confirmed { confirmed: true },
                },
                ack
            )
            .is_some(),
        "an unknown id is handed back for the loop's stale_request"
    );
}
